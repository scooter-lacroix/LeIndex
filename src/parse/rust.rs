// Rust language parser implementation

use crate::cfg_builder;
use crate::parse::traits::{
    find_node_by_id, Block, Edge, EdgeType, FlowChannel, FlowFact, Parameter, Visibility,
};
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
                        let body_node = node.child_by_field_name("body").unwrap_or(node);
                        let complexity_metrics = self.extract_complexity(&body_node);
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
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: qualified_name.clone(),
                            parameters: vec![],
                            return_type: Some("module".to_string()),
                            visibility: extract_visibility(&node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(&node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                            flow_facts: vec![],

                            cyclomatic_complexity: 0,
                        });

                        let mut new_path = parent_path.clone();
                        new_path.push(name.to_string());
                        push_children_with_path(&mut stack, node, &new_path);
                    } else {
                        push_children_with_path(&mut stack, node, &parent_path);
                    }
                }
                "impl_item" => {
                    self.extract_impl_definitions(&node, source, &parent_path, &mut signatures);
                }
                "trait_item" => {
                    if let Some(name) = node
                        .child_by_field_name("name")
                        .and_then(|n| n.utf8_text(source).ok())
                    {
                        let qualified_name = if parent_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}::{}", parent_path.join("::"), name)
                        };

                        signatures.push(SignatureInfo {
                            name: name.to_string(),
                            qualified_name: qualified_name.clone(),
                            parameters: vec![],
                            return_type: Some("trait".to_string()),
                            visibility: extract_visibility(&node, source),
                            is_async: false,
                            is_method: false,
                            docstring: extract_docstring(&node, source),
                            calls: vec![],
                            imports: vec![],
                            byte_range: (node.start_byte(), node.end_byte()),
                            flow_facts: vec![],

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
                            format!("{}::{}", parent_path.join("::"), name)
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
                            flow_facts: vec![],

                            cyclomatic_complexity: 0,
                        });
                    }
                }
                "enum_item" => {
                    Self::extract_enum_definitions(&node, source, &parent_path, &mut signatures);
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

    fn extract_impl_definitions(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        parent_path: &[String],
        signatures: &mut Vec<SignatureInfo>,
    ) {
        let impl_path = node
            .child_by_field_name("type")
            .and_then(|type_node| type_node.utf8_text(source).ok())
            .map(|text| text.split('<').next().unwrap_or(text).trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|type_name| {
                let mut path = parent_path.to_vec();
                path.push(type_name);
                path
            })
            .unwrap_or_else(|| parent_path.to_vec());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                let mut declarations = child.walk();
                for function in child.children(&mut declarations) {
                    if function.kind() == "function_item" {
                        self.push_impl_signature(&function, source, &impl_path, signatures);
                    }
                }
            } else if child.kind() == "function_item" {
                self.push_impl_signature(&child, source, &impl_path, signatures);
            }
        }
    }

    fn push_impl_signature(
        &self,
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        impl_path: &[String],
        signatures: &mut Vec<SignatureInfo>,
    ) {
        if let Some(mut sig) = extract_function_signature(node, source, impl_path) {
            // Every function declared in an impl belongs to the implementing type,
            // including associated functions that have no `self` parameter.
            sig.is_method = true;
            let body_node = node.child_by_field_name("body").unwrap_or(*node);
            let complexity_metrics = self.extract_complexity(&body_node);
            sig.cyclomatic_complexity = complexity_metrics.cyclomatic.max(1) as u32;
            signatures.push(sig);
        }
    }

    fn extract_enum_definitions(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        parent_path: &[String],
        signatures: &mut Vec<SignatureInfo>,
    ) {
        let Some(name) = node
            .child_by_field_name("name")
            .and_then(|name| name.utf8_text(source).ok())
        else {
            return;
        };

        let qualified_name = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", parent_path.join("::"), name)
        };

        signatures.push(SignatureInfo {
            name: name.to_string(),
            qualified_name: qualified_name.clone(),
            parameters: vec![],
            return_type: Some("enum".to_string()),
            visibility: extract_visibility(node, source),
            is_async: false,
            is_method: false,
            docstring: extract_docstring(node, source),
            calls: vec![],
            imports: vec![],
            byte_range: (node.start_byte(), node.end_byte()),
            flow_facts: vec![],
            cyclomatic_complexity: 0,
        });

        // Enum variants are stable review/search anchors even when a derive macro hides their
        // generated code.
        if let Some(body) = node.child_by_field_name("body") {
            let mut variants = body.walk();
            for variant in body.children(&mut variants) {
                if variant.kind() != "enum_variant" {
                    continue;
                }
                let Some(variant_name) = variant
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(source).ok())
                else {
                    continue;
                };
                signatures.push(SignatureInfo {
                    name: variant_name.to_string(),
                    qualified_name: format!("{}::{}", qualified_name, variant_name),
                    parameters: vec![],
                    return_type: Some("enum_variant".to_string()),
                    visibility: extract_visibility(node, source),
                    is_async: false,
                    is_method: false,
                    docstring: extract_docstring(&variant, source),
                    calls: vec![],
                    imports: vec![],
                    byte_range: (variant.start_byte(), variant.end_byte()),
                    flow_facts: vec![],
                    cyclomatic_complexity: 0,
                });
            }
        }
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
        format!("{}::{}", parent_path.join("::"), name)
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
        flow_facts: extract_rust_flow_facts(node, source),

        imports: vec![],
        byte_range: (node.start_byte(), node.end_byte()),

        cyclomatic_complexity: 0, // Will be populated by caller with extract_complexity
    })
}

/// Extract bounded, explicit value-flow facts from a Rust function body.
///
/// This intentionally stops at syntax that is cheap and reliable to identify:
/// call arguments, returns, common state verbs, and command-builder channels.
/// It does not attempt alias analysis, macro expansion, or type inference.
fn extract_rust_flow_facts(node: &tree_sitter::Node<'_>, source: &[u8]) -> Vec<FlowFact> {
    let mut facts = Vec::new();
    let mut stack = vec![*node];

    while let Some(current) = stack.pop() {
        match current.kind() {
            "function_item" if current.id() != node.id() => continue,
            "call_expression" => {
                extract_rust_call_flow_facts(&current, source, &mut facts);
            }
            "method_call_expression" => {
                extract_rust_method_flow_facts(&current, source, &mut facts);
            }
            "return_expression" => {
                if let Some(value) = current.child_by_field_name("body") {
                    facts.push(FlowFact {
                        channel: FlowChannel::ReturnValue,
                        source: clean_flow_label(value.utf8_text(source).unwrap_or("")),
                        target: "return".to_string(),
                        position: None,
                        byte_range: (value.start_byte(), value.end_byte()),
                    });
                }
            }
            "let_declaration" => {
                extract_rust_let_flow_facts(&current, source, &mut facts);
            }
            "assignment_expression" => {
                let left = current
                    .child_by_field_name("left")
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(clean_flow_label);
                let right = current
                    .child_by_field_name("right")
                    .and_then(|n| n.utf8_text(source).ok())
                    .map(clean_flow_label);
                if let (Some(left), Some(right)) = (left, right) {
                    facts.push(FlowFact {
                        channel: FlowChannel::ReturnValue,
                        source: right,
                        target: left,
                        position: None,
                        byte_range: (current.start_byte(), current.end_byte()),
                    });
                }
            }
            _ => {}
        }

        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            if child.kind() == "function_item" && child.id() != node.id() {
                continue;
            }
            stack.push(child);
        }
    }

    facts
}

fn extract_rust_call_flow_facts(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    facts: &mut Vec<FlowFact>,
) {
    let callee = node
        .child_by_field_name("function")
        .and_then(|name| name.utf8_text(source).ok())
        .map(clean_flow_label);
    let Some(callee) = callee else {
        return;
    };

    if callee == "Command::new" || callee.ends_with("::Command::new") {
        if let Some(arg) = expression_arguments(node, source).first() {
            facts.push(FlowFact {
                channel: FlowChannel::CommandArgument,
                source: clean_flow_label(arg.utf8_text(source).unwrap_or("")),
                target: "command".to_string(),
                position: Some(0),
                byte_range: (arg.start_byte(), arg.end_byte()),
            });
        }
    }

    let args = expression_arguments(node, source);
    for (position, arg) in args.into_iter().enumerate() {
        facts.push(FlowFact {
            channel: FlowChannel::Argument,
            source: clean_flow_label(arg.utf8_text(source).unwrap_or("")),
            target: callee.clone(),
            position: Some(position),
            byte_range: (arg.start_byte(), arg.end_byte()),
        });
    }

    if let Some(channel) = state_channel(&callee) {
        let args = expression_arguments(node, source);
        let source_label = args
            .first()
            .and_then(|arg| arg.utf8_text(source).ok())
            .map(clean_flow_label)
            .unwrap_or_else(|| callee.clone());
        facts.push(FlowFact {
            channel,
            source: source_label,
            target: callee.clone(),
            position: Some(0),
            byte_range: (node.start_byte(), node.end_byte()),
        });
    }

    // Older tree-sitter-rust grammars represent chained calls as nested call_expression nodes.
    // Recover command channels from the final method segment in that shape.
    if let Some(method) = chained_method_name(&callee) {
        let args = expression_arguments(node, source);
        match method {
            "arg" | "args" => {
                for (position, arg) in args.iter().enumerate() {
                    facts.push(FlowFact {
                        channel: FlowChannel::CommandArgument,
                        source: clean_flow_label(arg.utf8_text(source).unwrap_or("")),
                        target: "argv".to_string(),
                        position: Some(position),
                        byte_range: (arg.start_byte(), arg.end_byte()),
                    });
                }
            }
            "env" | "envs" => {
                let target = args
                    .first()
                    .and_then(|arg| arg.utf8_text(source).ok())
                    .map(clean_flow_label)
                    .unwrap_or_else(|| "env".to_string());
                let source_label = args
                    .get(1)
                    .or_else(|| args.first())
                    .and_then(|arg| arg.utf8_text(source).ok())
                    .map(clean_flow_label)
                    .unwrap_or_default();
                facts.push(FlowFact {
                    channel: FlowChannel::Environment,
                    source: source_label,
                    target,
                    position: Some(0),
                    byte_range: (node.start_byte(), node.end_byte()),
                });
            }
            "stdin" => {
                let source_label = args
                    .first()
                    .and_then(|arg| arg.utf8_text(source).ok())
                    .map(clean_flow_label)
                    .unwrap_or_else(|| "stdin".to_string());
                facts.push(FlowFact {
                    channel: FlowChannel::Stdin,
                    source: source_label.clone(),
                    target: source_label,
                    position: Some(0),
                    byte_range: (node.start_byte(), node.end_byte()),
                });
            }
            _ => {}
        }
    }
}

fn extract_rust_method_flow_facts(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    facts: &mut Vec<FlowFact>,
) {
    let method = node
        .child_by_field_name("method")
        .or_else(|| node.child_by_field_name("name"))
        .and_then(|name| name.utf8_text(source).ok())
        .map(clean_flow_label);
    let receiver = node
        .child_by_field_name("receiver")
        .and_then(|receiver| receiver.utf8_text(source).ok())
        .map(clean_flow_label)
        .unwrap_or_default();
    let args = method_arguments(node, source);

    let Some(method) = method else {
        return;
    };

    match method.as_str() {
        "arg" | "args" => {
            for (position, arg) in args.iter().enumerate() {
                facts.push(FlowFact {
                    channel: FlowChannel::CommandArgument,
                    source: clean_flow_label(arg.utf8_text(source).unwrap_or("")),
                    target: "argv".to_string(),
                    position: Some(position),
                    byte_range: (arg.start_byte(), arg.end_byte()),
                });
            }
        }
        "env" | "envs" => {
            let target = args
                .first()
                .and_then(|arg| arg.utf8_text(source).ok())
                .map(clean_flow_label)
                .unwrap_or_else(|| "env".to_string());
            let source_label = args
                .get(1)
                .or_else(|| args.first())
                .and_then(|arg| arg.utf8_text(source).ok())
                .map(clean_flow_label)
                .unwrap_or_default();
            facts.push(FlowFact {
                channel: FlowChannel::Environment,
                source: source_label,
                target,
                position: Some(0),
                byte_range: (node.start_byte(), node.end_byte()),
            });
        }
        "stdin" => {
            let source_label = args
                .first()
                .and_then(|arg| arg.utf8_text(source).ok())
                .map(clean_flow_label)
                .unwrap_or_else(|| "stdin".to_string());
            facts.push(FlowFact {
                channel: FlowChannel::Stdin,
                source: source_label.clone(),
                target: source_label,
                position: Some(0),
                byte_range: (node.start_byte(), node.end_byte()),
            });
        }
        _ => {
            if let Some(channel) = state_channel(&method) {
                facts.push(FlowFact {
                    channel,
                    source: receiver,
                    target: method,
                    position: None,
                    byte_range: (node.start_byte(), node.end_byte()),
                });
            }
        }
    }
}

fn extract_rust_let_flow_facts(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    facts: &mut Vec<FlowFact>,
) {
    let pattern = node
        .child_by_field_name("pattern")
        .and_then(|pattern| pattern.utf8_text(source).ok())
        .map(clean_flow_label);
    let value = node
        .child_by_field_name("value")
        .and_then(|value| value.utf8_text(source).ok())
        .map(clean_flow_label);
    let (Some(pattern), Some(value)) = (pattern, value) else {
        return;
    };

    if value.contains("Command::new") {
        if let Some(command) = value
            .split("Command::new(")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
        {
            facts.push(FlowFact {
                channel: FlowChannel::CommandArgument,
                source: clean_flow_label(command),
                target: pattern,
                position: Some(0),
                byte_range: (node.start_byte(), node.end_byte()),
            });
        }
    } else if value.contains('(') {
        facts.push(FlowFact {
            channel: FlowChannel::ReturnValue,
            source: value,
            target: pattern,
            position: None,
            byte_range: (node.start_byte(), node.end_byte()),
        });
    }
}

fn expression_arguments<'a>(
    node: &tree_sitter::Node<'a>,
    _source: &[u8],
) -> Vec<tree_sitter::Node<'a>> {
    node.child_by_field_name("arguments")
        .map(|args| {
            let mut cursor = args.walk();
            args.children(&mut cursor)
                .filter(|child| !matches!(child.kind(), "," | "(" | ")"))
                .collect()
        })
        .unwrap_or_default()
}

fn method_arguments<'a>(node: &tree_sitter::Node<'a>, source: &[u8]) -> Vec<tree_sitter::Node<'a>> {
    expression_arguments(node, source)
}

fn clean_flow_label(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .trim_end_matches(';')
        .to_string()
}

fn state_channel(name: &str) -> Option<FlowChannel> {
    let verb = name
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    if matches!(
        verb.as_str(),
        "insert"
            | "write"
            | "set"
            | "record"
            | "save"
            | "update"
            | "render"
            | "publish"
            | "emit"
            | "seal"
    ) || verb.contains("record")
    {
        Some(FlowChannel::StateWrite)
    } else if matches!(verb.as_str(), "verify" | "get" | "load" | "read" | "fetch")
        || verb.contains("verify")
    {
        Some(FlowChannel::StateRead)
    } else {
        None
    }
}

fn chained_method_name(callee: &str) -> Option<&str> {
    let method = callee.rsplit('.').next()?;
    matches!(method, "arg" | "args" | "env" | "envs" | "stdin").then_some(method)
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

    fn extract_call_expression(
        node: &tree_sitter::Node<'_>,
        source: &[u8],
        calls: &mut Vec<String>,
        clean_call_text: fn(&str) -> String,
    ) {
        if let Some(func) = node.child_by_field_name("function") {
            if let Ok(text) = func.utf8_text(source) {
                let name = clean_call_text(text);
                if !name.is_empty() {
                    calls.push(name);
                }
            }

            // Extract the type prefix from scoped calls like Foo::new().
            if func.kind() == "scoped_identifier" {
                if let Some(path_node) = func.child_by_field_name("path") {
                    if let Ok(path_text) = path_node.utf8_text(source) {
                        if let Some(type_name) = normalize_type_ref(path_text) {
                            calls.push(type_name);
                        }
                    }
                }
            }
        }
    }

    let mut stack = vec![*node];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "call_expression" => {
                extract_call_expression(&current, source, &mut calls, clean_call_text);
            }
            "method_call_expression" => {
                let receiver = current
                    .child_by_field_name("receiver")
                    .and_then(|r| r.utf8_text(source).ok())
                    .map(clean_call_text);
                let method = current
                    .child_by_field_name("method")
                    .and_then(|m| m.utf8_text(source).ok())
                    .map(clean_call_text);

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
                        if let Some(type_name) = normalize_type_ref(name) {
                            calls.push(type_name);
                        }
                    }
                }
            }
            _ => {}
        }

        let mut cursor = current.walk();
        let children: Vec<_> = current.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            // Skip nested function_item nodes — their calls belong to themselves
            if child.kind() == "function_item" {
                continue;
            }
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
        flow_facts: vec![],

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

/// Calculate complexity metrics (iterative to avoid stack overflow on deeply nested code)
fn calculate_complexity(
    node: &tree_sitter::Node<'_>,
    metrics: &mut ComplexityMetrics,
    depth: usize,
) {
    // Use a stack-based approach with explicit traversal to avoid recursion
    // Stack holds (node, depth) pairs
    let mut stack: Vec<(tree_sitter::Node<'_>, usize)> = Vec::new();
    stack.push((*node, depth));

    while let Some((current_node, current_depth)) = stack.pop() {
        metrics.nesting_depth = metrics.nesting_depth.max(current_depth);
        metrics.line_count = std::cmp::max(metrics.line_count, 1);

        match current_node.kind() {
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
                if let Some(op) = current_node.child_by_field_name("operator") {
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

        metrics.token_count += current_node.child_count();

        // Push children onto stack in reverse order to process them left-to-right
        let mut cursor = current_node.walk();
        let mut children: Vec<tree_sitter::Node<'_>> = current_node.children(&mut cursor).collect();
        children.reverse(); // Reverse to maintain left-to-right processing order

        for child in children {
            // Skip nested function_item nodes that appear inside a block (function body).
            // Top-level function_items (children of source_file) are always traversed.
            if child.kind() == "function_item" && current_node.kind() == "block" {
                continue;
            }
            stack.push((child, current_depth + 1));
        }
    }
}

cfg_builder!();
impl<'a> CfgBuilder<'a> {
    fn build_from_node(&mut self, node: &tree_sitter::Node<'_>) -> Result<()> {
        let entry_id = self.create_block();
        self.build_cfg_iterative(node, entry_id)?;
        Ok(())
    }

    fn build_cfg_iterative(
        &mut self,
        root_node: &tree_sitter::Node<'_>,
        entry_block: usize,
    ) -> Result<()> {
        use std::collections::VecDeque;

        // Work queue: (node, current_block_id)
        let mut work_queue: VecDeque<(tree_sitter::Node<'_>, usize)> = VecDeque::new();
        work_queue.push_back((*root_node, entry_block));

        while let Some((node, current_block)) = work_queue.pop_front() {
            match node.kind() {
                "if_expression" | "if_let_expression" => {
                    self.handle_if_statement(&node, current_block)?;
                }
                "while_expression"
                | "while_let_expression"
                | "for_expression"
                | "loop_expression" => {
                    self.handle_loop_statement(&node, current_block)?;
                }
                "match_expression" => {
                    self.handle_match_statement(&node, current_block)?;
                }
                _ => {
                    if let Ok(text) = node.utf8_text(self.source) {
                        self.add_statement_to_block(current_block, text.to_string());
                    }

                    // Add children to the work queue
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        work_queue.push_back((child, current_block));
                    }
                }
            }
        }

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
}

/// Normalize a type reference: strip turbofish generics (`::<...>`),
/// take the terminal segment after the last `::`, and verify it starts uppercase.
fn normalize_type_ref(raw: &str) -> Option<String> {
    let stripped = raw.split("::<").next().unwrap_or(raw);
    let last = stripped.rsplit("::").next().unwrap_or(stripped).trim();
    last.chars().next().filter(|c| c.is_uppercase())?;
    Some(last.to_string())
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
        assert!(methods
            .iter()
            .any(|sig| sig.qualified_name == "Server::new"));
        assert!(methods
            .iter()
            .any(|sig| sig.qualified_name == "Server::start"));
        assert!(methods
            .iter()
            .any(|sig| sig.qualified_name == "Server::connect"));
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
        assert!(signatures
            .iter()
            .any(|s| s.qualified_name == "Option::Some"));
        assert!(signatures
            .iter()
            .any(|s| s.qualified_name == "Option::None"));
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
        assert!(
            metrics.cyclomatic >= 8,
            "Expected cyclomatic complexity >= 8, got {}",
            metrics.cyclomatic
        );
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
        assert!(
            metrics.cyclomatic >= 3,
            "Expected cyclomatic complexity >= 3, got {}",
            metrics.cyclomatic
        );
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
        assert_eq!(
            metrics.cyclomatic, 5,
            "Expected cyclomatic complexity = 5, got {}",
            metrics.cyclomatic
        );
    }

    #[test]
    fn test_rust_complexity_with_try_and_bool() {
        let source =
            b"fn try_and_bool(x: Result<i32, Error>, y: Result<i32, Error>) -> Result<i32, Error> {
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
        assert_eq!(
            metrics.cyclomatic, 5,
            "Expected cyclomatic complexity = 5, got {}",
            metrics.cyclomatic
        );
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
        assert!(
            create_structs_fn.is_some(),
            "create_structs function not found"
        );

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
            fn_sig
                .calls
                .iter()
                .any(|c| c.contains("DeepThoughtManager::new")),
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

    #[test]
    fn test_rust_mod_item_extraction() {
        let source = b"
pub mod outer {
    mod inner {
        fn baz() {}
    }
}
mod external;
";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Module signatures should be present
        let outer = signatures.iter().find(|s| s.name == "outer");
        assert!(outer.is_some(), "should emit SignatureInfo for `outer`");
        assert_eq!(outer.unwrap().return_type.as_deref(), Some("module"));
        assert_eq!(outer.unwrap().visibility, Visibility::Public);

        let inner = signatures.iter().find(|s| s.name == "inner");
        assert!(
            inner.is_some(),
            "should emit SignatureInfo for nested `inner`"
        );
        assert_eq!(inner.unwrap().return_type.as_deref(), Some("module"));

        // `mod external;` should also produce a module signature
        let ext = signatures.iter().find(|s| s.name == "external");
        assert!(
            ext.is_some(),
            "should emit SignatureInfo for external `mod external;`"
        );

        // Function inside nested module should be indexed
        let baz = signatures.iter().find(|s| s.name == "baz");
        assert!(baz.is_some(), "nested fn baz should be indexed");
        // qualified_name should reflect the module path exactly
        assert_eq!(
            baz.unwrap().qualified_name,
            "outer::inner::baz",
            "baz qualified_name should match the module path exactly"
        );
    }

    #[test]
    fn test_rust_flow_facts_capture_command_and_state_channels() {
        let source = br#"
fn execute_native_command(password: &str, askpass: &str) {
    let mut command = std::process::Command::new("sudo");
    command.arg("-S").env("SUDO_ASKPASS", askpass).stdin(password);
    registry_record(password);
    verify_installation();
}
"#;
        let signatures = RustParser::new().get_signatures(source).unwrap();
        let sig = signatures
            .iter()
            .find(|sig| sig.name == "execute_native_command")
            .unwrap();

        assert!(sig
            .flow_facts
            .iter()
            .any(|fact| { fact.channel == FlowChannel::CommandArgument && fact.target == "argv" }));
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::Environment && fact.target == "SUDO_ASKPASS"
        }));
        assert!(sig
            .flow_facts
            .iter()
            .any(|fact| { fact.channel == FlowChannel::Stdin && fact.target == "password" }));
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::StateWrite && fact.target == "registry_record"
        }));
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::StateRead && fact.target == "verify_installation"
        }));
    }
}
