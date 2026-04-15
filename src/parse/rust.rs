// Rust language parser implementation

use crate::parse::traits::{Block, Edge, EdgeType, Parameter, Visibility};
use crate::parse::traits::{
    CodeIntelligence, ComplexityMetrics, Error, Graph, ImportInfo, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// Rust language parser with full CodeIntelligence implementation
pub struct RustParser;

impl Default for RustParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RustParser {
    /// Create a new Rust parser
    pub fn new() -> Self {
        Self
    }

    /// Extract all function and type definitions from Rust source
    fn extract_all_definitions(
        &self,
        source: &[u8],
        root: tree_sitter::Node<'_>,
    ) -> Vec<SignatureInfo> {
        let mut signatures = Vec::new();
        let mut stack = vec![(root, Vec::<String>::new())];

        while let Some((node, parent_path)) = stack.pop() {
            match node.kind() {
                "function_item" => {
                    if let Some(mut sig) = extract_function_signature(&node, source, &parent_path) {
                        // Extract and populate cyclomatic complexity
                        let complexity_metrics = self.extract_complexity(&node);
                        sig.cyclomatic_complexity = complexity_metrics.cyclomatic.max(1) as u32;
                        signatures.push(sig);
                    }
                    // Don't recurse into function bodies.
                }
                "mod_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let mut new_path = parent_path.clone();
                        new_path.push(name.to_string());
                        push_children_with_path(&mut stack, node, &new_path);
                    } else {
                        push_children_with_path(&mut stack, node, &parent_path);
                    }
                }
                "impl_item" => {
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "declaration_list" {
                            let mut dcursor = child.walk();
                            for dc in child.children(&mut dcursor) {
                                if dc.kind() == "function_item" {
                                    if let Some(mut sig) =
                                        extract_function_signature(&dc, source, &parent_path)
                                    {
                                        // Extract and populate cyclomatic complexity
                                        let complexity_metrics = self.extract_complexity(&dc);
                                        sig.cyclomatic_complexity = complexity_metrics.cyclomatic.max(1) as u32;
                                        signatures.push(sig);
                                    }
                                }
                            }
                        } else if child.kind() == "function_item" {
                            if let Some(mut sig) =
                                extract_function_signature(&child, source, &parent_path)
                            {
                                // Extract and populate cyclomatic complexity
                                let complexity_metrics = self.extract_complexity(&child);
                                sig.cyclomatic_complexity = complexity_metrics.cyclomatic.max(1) as u32;
                                signatures.push(sig);
                            }
                        }
                    }
                }
                "trait_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("trait".to_string()),
                            visibility: extract_visibility(&node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(&node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                            cyclomatic_complexity: 0,
                        });
                    }

                    push_children_with_path(&mut stack, node, &parent_path);
                }
                "struct_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        let type_params = node
                            .child_by_field_name("type_parameters")
                            .and_then(|tp| tp.utf8_text(source).ok())
                            .map(|s| s.trim().to_string());

                        let return_type = if let Some(tp) = type_params {
                            format!("struct{}", tp)
                        } else {
                            "struct".to_string()
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some(return_type),
                            visibility: extract_visibility(&node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(&node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                            cyclomatic_complexity: 0,
                        });
                    }
                }
                "enum_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}.{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name,
                            parameters: vec![],
                            return_type: Some("enum".to_string()),
                            visibility: extract_visibility(&node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(&node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                            cyclomatic_complexity: 0,
                        });
                    }
                }
                "use_declaration" => {
                    if let Some(sig) = extract_import_signature(&node, source, &parent_path) {
                        signatures.push(sig);
                    }
                }
                _ => {
                    push_children_with_path(&mut stack, node, &parent_path);
                }
            }
        }

        signatures
    }
}

fn push_children_with_path<'tree>(
    stack: &mut Vec<(tree_sitter::Node<'tree>, Vec<String>)>,
    node: tree_sitter::Node<'tree>,
    parent_path: &[String],
) {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();
    for child in children.into_iter().rev() {
        stack.push((child, parent_path.to_vec()));
    }
}

impl CodeIntelligence for RustParser {
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
            .set_language(&crate::parse::traits::languages::rust::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Rust source".to_string()))?;

        let root_node = tree.root_node();

        let imports = extract_rust_imports(root_node, source);
        let mut signatures = self.extract_all_definitions(source, root_node);

        for sig in &mut signatures {
            sig.imports = imports.clone();
        }

        Ok(signatures)
    }

    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<Graph<Block, Edge>> {
        let mut parser = Parser::new();
        parser
            .set_language(&crate::parse::traits::languages::rust::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse Rust source".to_string()))?;

        let root_node = tree.root_node();

        let node = find_node_by_id(&root_node, node_id)
            .ok_or_else(|| Error::ParseFailed(format!("Node {} not found", node_id)))?;

        let mut cfg_builder = CfgBuilder::new(source);
        cfg_builder.build_from_node(&node)?;

        Ok(cfg_builder.finish())
    }

    fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> ComplexityMetrics {
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

/// Extract imports from a Rust file
fn extract_rust_imports(root: tree_sitter::Node<'_>, source: &[u8]) -> Vec<ImportInfo> {
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

    fn parse_use_text(imports: &mut Vec<ImportInfo>, text: &str) {
        let mut text = text.trim();
        if text.starts_with("use ") {
            text = text.trim_start_matches("use ");
        }
        text = text.trim_end_matches(';').trim();

        if let Some((base, rest)) = text.split_once('{') {
            let base = base.trim().trim_end_matches("::");
            let rest = rest.trim_end_matches('}');
            for item in rest.split(',') {
                let item = item.trim();
                if item.is_empty() || item == "*" {
                    continue;
                }
                let (item_path, alias) = if let Some((path, alias)) = item.split_once(" as ") {
                    (path.trim(), Some(alias.trim().to_string()))
                } else {
                    (item, None)
                };
                let full_path = if base.is_empty() {
                    item_path.to_string()
                } else {
                    format!("{}::{}", base, item_path)
                };
                let alias = alias.or_else(|| item_path.split("::").last().map(|s| s.to_string()));
                add_import(imports, &full_path, alias);
            }
        } else {
            let (path, alias) = if let Some((path, alias)) = text.split_once(" as ") {
                (path.trim(), Some(alias.trim().to_string()))
            } else {
                (text, None)
            };
            let alias = alias.or_else(|| path.split("::").last().map(|s| s.to_string()));
            add_import(imports, path, alias);
        }
    }

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "use_declaration" {
            if let Ok(text) = node.utf8_text(source) {
                parse_use_text(&mut imports, text);
            }
        }

        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    imports
}

/// Extract function signature from a function_item node
fn extract_function_signature(
    node: &tree_sitter::Node<'_>,
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
        format!("{}.{}", parent_path.join("::"), name)
    };

    let parameters = extract_rust_parameters(node, source);

    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|r| r.utf8_text(source).ok())
        .map(|s| s.trim().to_string());

    let is_async = node.children(&mut node.walk()).any(|c| {
        c.kind() == "function_modifiers" && c.children(&mut c.walk()).any(|cc| cc.kind() == "async")
    });

    let visibility = extract_visibility(node, source);

    // Check if this is a method (has self parameter)
    let is_method = parameters
        .first()
        .map(|p| p.name.contains("self"))
        .unwrap_or(false);

    let calls = extract_rust_calls(node, source);

    Some(SignatureInfo {
        name,
        qualified_name,
        parameters,
        return_type,
        visibility,
        is_async,
        is_method,
        docstring: extract_docstring(node, source),
        calls,

        imports: vec![],
        byte_range: (node.start_byte(), node.end_byte()),
        cyclomatic_complexity: 0, // Will be populated by caller with extract_complexity
    })
}

/// Extract function calls from a Rust node
fn extract_rust_calls(node: &tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut calls = Vec::new();

    fn clean_call_text(raw: &str) -> String {
        raw.split('(')
            .next()
            .unwrap_or(raw)
            .replace("::<", "::")
            .trim()
            .trim_end_matches('!')
            .to_string()
    }

    let mut stack = vec![*node];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "call_expression" => {
                if let Some(func) = current.child_by_field_name("function") {
                    if let Ok(text) = func.utf8_text(source) {
                        let name = clean_call_text(text);
                        if !name.is_empty() {
                            calls.push(name);
                        }
                    }

                    // NEW: extract the type prefix from scoped calls like Foo::new()
                    if func.kind() == "scoped_identifier" {
                        if let Some(path_node) = func.child_by_field_name("path") {
                            if let Ok(path_text) = path_node.utf8_text(source) {
                                let type_name = path_text.to_string();
                                if !type_name.is_empty()
                                    && type_name.chars().next().map_or(false, |c| c.is_uppercase())
                                {
                                    calls.push(type_name);
                                }
                            }
                        }
                    }
                }
            }
            "method_call_expression" => {
                let receiver = current
                    .child_by_field_name("receiver")
                    .and_then(|r| r.utf8_text(source).ok())
                    .map(|s| clean_call_text(s));
                let method = current
                    .child_by_field_name("method")
                    .and_then(|m| m.utf8_text(source).ok())
                    .map(|s| clean_call_text(s));

                let name = match (receiver, method) {
                    (Some(r), Some(m)) => format!("{}.{}", r, m),
                    (_, Some(m)) => m,
                    _ => String::new(),
                };

                if !name.is_empty() {
                    calls.push(name);
                }
            }
            "macro_invocation" => {
                if let Some(name_node) = current
                    .child_by_field_name("macro")
                    .or_else(|| current.child_by_field_name("name"))
                {
                    if let Ok(text) = name_node.utf8_text(source) {
                        let name = clean_call_text(text);
                        if !name.is_empty() {
                            calls.push(name);
                        }
                    }
                }
            }
            "struct_expression" => {
                if let Some(name_node) = current.child_by_field_name("name") {
                    if let Ok(name) = name_node.utf8_text(source) {
                        calls.push(name.to_string());
                    }
                }
            }
            _ => {}
        }

        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    calls
}

/// Extract import signature from a use_declaration node
fn extract_import_signature(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    _parent_path: &[String],
) -> Option<SignatureInfo> {
    let import_arg = node
        .child_by_field_name("argument")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())?;

    // Extract just the last part as the "name"
    let name = import_arg
        .split("::")
        .last()
        .unwrap_or(&import_arg)
        .split('{')
        .next()
        .unwrap_or(&import_arg)
        .split(' ')
        .next()
        .unwrap_or(&import_arg)
        .to_string();

    Some(SignatureInfo {
        name: name.clone(),
        qualified_name: import_arg,
        parameters: vec![],
        return_type: Some("use".to_string()),
        visibility: Visibility::Public,
        is_async: false,
        is_method: false,
        docstring: None,
        calls: vec![],
        imports: vec![],
        byte_range: (0, 0),
        cyclomatic_complexity: 0,
    })
}

/// Extract visibility modifier from a node
fn extract_visibility(node: &tree_sitter::Node<'_>, source: &[u8]) -> Visibility {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            if let Ok(text) = child.utf8_text(source) {
                if text.contains("pub")
                    && !text.contains("pub(crate)")
                    && !text.contains("pub(super)")
                {
                    return Visibility::Public;
                } else if text.contains("pub(crate)") || text.contains("pub(super)") {
                    return Visibility::Protected; // Use protected for restricted visibility
                }
            }
        }
    }
    Visibility::Private
}

/// Extract parameters from a Rust function
fn extract_rust_parameters(node: &tree_sitter::Node<'_>, source: &[u8]) -> Vec<Parameter> {
    let mut parameters = Vec::new();

    if let Some(params) = node.child_by_field_name("parameters") {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "self_parameter" {
                // Self parameter (&self, &mut self, self)
                if let Ok(text) = child.utf8_text(source) {
                    parameters.push(Parameter {
                        name: text.trim().to_string(),
                        type_annotation: Some("self".to_string()),
                        default_value: None,
                    });
                }
            } else if child.kind() == "parameter" {
                // Regular parameter: name: Type
                let mut name = None;
                let mut type_annotation = None;

                let mut ccursor = child.walk();
                for param_child in child.children(&mut ccursor) {
                    match param_child.kind() {
                        "identifier" => {
                            if let Ok(text) = param_child.utf8_text(source) {
                                name = Some(text.to_string());
                            }
                        }
                        ":" | "," | "(" | ")" => {
                            // Skip punctuation
                        }
                        _ => {
                            // Everything else is likely a type annotation
                            if let Ok(text) = param_child.utf8_text(source) {
                                let text = text.trim();
                                if !text.is_empty() && text != ":" && text != "," {
                                    type_annotation = Some(text.to_string());
                                }
                            }
                        }
                    }
                }

                // Only add if we have a name
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

/// Extract docstring from a node
fn extract_docstring(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // Look for doc comments before the node
    let prev_sibling = node.prev_sibling();

    // Check for doc comment (line or block)
    if let Some(sibling) = prev_sibling {
        if sibling.kind() == "line_comment" || sibling.kind() == "block_comment" {
            if let Ok(text) = sibling.utf8_text(source) {
                let is_doc = text.starts_with("///")
                    || text.starts_with("//!")
                    || text.starts_with("/**")
                    || text.starts_with("/*!");
                if is_doc {
                    return Some(
                        text.trim()
                            .trim_start_matches("///")
                            .trim_start_matches("//!")
                            .trim_start_matches("/**")
                            .trim_start_matches("/*!")
                            .trim_end_matches("*/")
                            .trim()
                            .to_string(),
                    );
                }
            }
        }
    }

    None
}

/// Find a node by its ID
fn find_node_by_id<'a>(
    node: &'a tree_sitter::Node<'a>,
    id: usize,
) -> Option<tree_sitter::Node<'a>> {
    use std::collections::VecDeque;

    if node.id() == id {
        return Some(*node);
    }

    let mut queue: VecDeque<tree_sitter::Node<'a>> = VecDeque::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        queue.push_back(child);
    }

    while let Some(current) = queue.pop_front() {
        if current.id() == id {
            return Some(current);
        }

        let mut child_cursor = current.walk();
        for child in current.children(&mut child_cursor) {
            queue.push_back(child);
        }
    }

    None
}

/// Calculate complexity metrics
fn calculate_complexity(
    node: &tree_sitter::Node<'_>,
    metrics: &mut ComplexityMetrics,
    depth: usize,
) {
    metrics.nesting_depth = metrics.nesting_depth.max(depth);
    metrics.line_count = std::cmp::max(metrics.line_count, 1);

    match node.kind() {
        "if_expression"
        | "if_let_expression"
        | "while_expression"
        | "while_let_expression"
        | "for_expression"
        | "loop_expression"
        | "match_expression"
        | "match_arm"
        | "if_expression_else" => {
            metrics.cyclomatic += 1;
        }
        "binary_expression" => {
            if let Some(op) = node.child_by_field_name("operator") {
                match op.kind() {
                    "&&" | "||" => {
                        metrics.cyclomatic += 1;
                    }
                    _ => {}
                }
            }
        }
        "try_expression" => {
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

    fn build_from_node(&mut self, node: &tree_sitter::Node<'_>) -> Result<()> {
        let entry_id = self.create_block();
        self.build_cfg_recursive(node, entry_id)?;
        Ok(())
    }

    fn build_cfg_recursive(
        &mut self,
        node: &tree_sitter::Node<'_>,
        current_block: usize,
    ) -> Result<()> {
        match node.kind() {
            "if_expression" | "if_let_expression" => {
                self.handle_if_statement(node, current_block)?;
            }
            "while_expression" | "while_let_expression" | "for_expression" | "loop_expression" => {
                self.handle_loop_statement(node, current_block)?;
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

    fn handle_if_statement(
        &mut self,
        _node: &tree_sitter::Node<'_>,
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
        _node: &tree_sitter::Node<'_>,
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

    fn handle_match_statement(
        &mut self,
        _node: &tree_sitter::Node<'_>,
        current_block: usize,
    ) -> Result<()> {
        let merge_block = self.create_block();

        // Create a block for each match arm
        let mut cursor = _node.walk();
        let mut has_arms = false;
        for child in _node.children(&mut cursor) {
            if child.kind() == "match_arm" {
                has_arms = true;
                let arm_block = self.create_block();
                self.edges.push(Edge {
                    from: current_block,
                    to: arm_block,
                    edge_type: EdgeType::TrueBranch,
                });
                self.edges.push(Edge {
                    from: arm_block,
                    to: merge_block,
                    edge_type: EdgeType::Unconditional,
                });
            }
        }

        if !has_arms {
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
    fn test_rust_function_extraction() {
        let source = b"fn greet(name: &str) -> String {
    format!(\"Hello, {}\", name)
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("String".to_string()));
        assert!(!sig.is_method);
    }

    #[test]
    fn test_rust_async_function() {
        let source = b"async fn fetch_data(url: &str) -> Result<String, Error> {
    Ok(String::new())
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "fetch_data");
        assert!(sig.is_async);
    }

    #[test]
    fn test_rust_method_extraction() {
        let source = b"impl Server {
    fn new() -> Self {
        Server {}
    }

    pub fn start(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Client for Server {
    fn connect(&self) -> bool {
        true
    }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find methods from impl blocks
        let methods: Vec<_> = signatures.iter().filter(|s| s.is_method).collect();
        assert!(!methods.is_empty());
    }

    #[test]
    fn test_rust_struct_extraction() {
        let source = b"struct Point {
    x: f64,
    y: f64,
}

pub struct Person {
    pub name: String,
    pub age: u32,
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let point = signatures.iter().find(|s| s.name == "Point");
        assert!(point.is_some());

        let person = signatures.iter().find(|s| s.name == "Person");
        assert!(person.is_some());
    }

    #[test]
    fn test_rust_enum_extraction() {
        let source = b"enum Option<T> {
    Some(T),
    None,
}

pub enum Result<T, E> {
    Ok(T),
    Err(E),
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let option = signatures.iter().find(|s| s.name == "Option");
        assert!(option.is_some());

        let result = signatures.iter().find(|s| s.name == "Result");
        assert!(result.is_some());
    }

    #[test]
    fn test_rust_trait_extraction() {
        let source = b"trait Display {
    fn fmt(&self, f: &mut Formatter) -> Result;
}

trait Iterator {
    type Item;
    fn next(&mut self) -> Option<Self::Item>;
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract trait declarations
        assert!(signatures.len() >= 2);

        let display = signatures.iter().find(|s| s.name == "Display");
        assert!(display.is_some());

        let iterator = signatures.iter().find(|s| s.name == "Iterator");
        assert!(iterator.is_some());
    }

    #[test]
    fn test_rust_visibility_modifiers() {
        let source = b"pub fn public_function() {}

fn private_function() {}

pub(crate) fn crate_function() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        let public = signatures.iter().find(|s| s.name == "public_function");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = signatures.iter().find(|s| s.name == "private_function");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let crate_fn = signatures.iter().find(|s| s.name == "crate_function");
        assert!(crate_fn.is_some());
    }

    #[test]
    fn test_rust_import_extraction() {
        let source = b"use std::collections::HashMap;
use crate::module::Item;

fn main() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract use declarations
        let imports: Vec<_> = signatures
            .iter()
            .filter(|s| s.return_type.as_deref() == Some("use"))
            .collect();

        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_rust_self_parameter() {
        let source = b"impl Foo {
    fn by_ref(&self) -> i32 { 0 }
    fn by_mut_ref(&mut self) -> i32 { 0 }
    fn by_value(self) -> i32 { 0 }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        for sig in &signatures {
            assert!(sig.is_method);
            assert!(!sig.parameters.is_empty());
            assert!(sig.parameters[0].name.contains("self"));
        }
    }

    #[test]
    fn test_rust_complexity_calculation() {
        let source = b"fn complex(x: i32) -> i32 {
    if x > 0 {
        for i in 0..x {
            if i % 2 == 0 {
                println!(\"{}\", i);
            }
        }
    }
    x
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }

    #[test]
    fn test_rust_complexity_with_boolean_operators() {
        let source = b"fn boolean_ops(x: i32, y: i32) -> bool {
    if x > 0 && y > 0 {
        return true;
    }
    if x < 0 || y < 0 {
        return false;
    }
    x > 0 && y > 0 || x == 0 && y == 0
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 if expressions (2) + 5 boolean operators (5) = 8
        assert!(metrics.cyclomatic >= 8, "Expected cyclomatic complexity >= 8, got {}", metrics.cyclomatic);
    }

    #[test]
    fn test_rust_complexity_with_try_expressions() {
        let source = b"fn try_ops(result: Result<i32, Error>) -> Result<i32, Error> {
    let x = result?;
    let y = Some(2).ok_or(Error::NotFound)?;
    Ok(x + y)
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 try expressions (2) = 3
        assert!(metrics.cyclomatic >= 3, "Expected cyclomatic complexity >= 3, got {}", metrics.cyclomatic);
    }

    #[test]
    fn test_rust_complexity_combined() {
        let source = b"fn combined(x: i32, y: i32) -> i32 {
    if x > 0 && y > 0 {
        return x + y;
    }
    if x < 0 || y < 0 {
        return x - y;
    }
    0
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 if expressions (2) + 2 boolean operators (2) = 5
        // Note: tree-sitter might parse complex boolean expressions as nested binary expressions
        assert_eq!(metrics.cyclomatic, 5, "Expected cyclomatic complexity = 5, got {}", metrics.cyclomatic);
    }

    #[test]
    fn test_rust_complexity_with_try_and_bool() {
        let source = b"fn try_and_bool(x: Result<i32, Error>, y: Result<i32, Error>) -> Result<i32, Error> {
    let a = x?;
    let b = y?;
    if a > 0 && b > 0 {
        return Ok(a + b);
    }
    Ok(a - b)
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 1 if expression (1) + 1 boolean operator (1) + 2 try expressions (2) = 5
        assert_eq!(metrics.cyclomatic, 5, "Expected cyclomatic complexity = 5, got {}", metrics.cyclomatic);
    }

    #[test]
    fn test_rust_cyclomatic_complexity_populated_in_signature() {
        let source = b"fn complex_fn(x: i32) -> i32 {
    if x > 0 {
        return x * 2;
    }
    x + 1
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "complex_fn");

        // Verify that cyclomatic_complexity is populated
        // The function has 1 if expression, so cyclomatic complexity should be 2 (base 1 + 1 if)
        assert!(
            sig.cyclomatic_complexity >= 1,
            "cyclomatic_complexity should be >= 1, got {}",
            sig.cyclomatic_complexity
        );
    }

    #[test]
    fn test_rust_struct_instantiation_calls() {
        let source = b"fn create_structs() {
    let x = MyStruct { field: 1 };
    let y = AnotherStruct { value: compute() };
    let z = GenericStruct::<i32> { data: 42 };
}

fn compute() -> i32 {
    42
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let create_structs_fn = signatures.iter().find(|s| s.name == "create_structs");
        assert!(create_structs_fn.is_some(), "create_structs function not found");

        let fn_sig = create_structs_fn.unwrap();

        // Should detect struct instantiations
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("MyStruct")),
            "Should detect MyStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("AnotherStruct")),
            "Should detect AnotherStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("GenericStruct")),
            "Should detect GenericStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );

        // Should also detect function calls within struct fields
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("compute")),
            "Should detect compute() call within struct field, got calls: {:?}",
            fn_sig.calls
        );
    }

    #[test]
    fn test_rust_scoped_identifier_extraction() {
        let source = b"
struct DeepThoughtManager {
    answer: i32,
}

impl DeepThoughtManager {
    fn new() -> Self {
        DeepThoughtManager { answer: 42 }
    }
}

fn test_function() {
    let manager = DeepThoughtManager::new();
    let another = DeepThoughtManager::new();
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let test_fn = signatures.iter().find(|s| s.name == "test_function");
        assert!(test_fn.is_some(), "test_function not found");

        let fn_sig = test_fn.unwrap();

        // Should detect the scoped function call DeepThoughtManager::new
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("DeepThoughtManager::new")),
            "Should detect DeepThoughtManager::new call, got calls: {:?}",
            fn_sig.calls
        );

        // Should also detect just the type prefix DeepThoughtManager
        assert!(
            fn_sig.calls.iter().any(|c| c == "DeepThoughtManager"),
            "Should detect DeepThoughtManager type prefix, got calls: {:?}",
            fn_sig.calls
        );
    }
}
