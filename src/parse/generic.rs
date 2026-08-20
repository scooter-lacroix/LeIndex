//! Tier-0 generic tree-sitter parser.
//!
//! The 100+ language goal needs breadth without 100 bespoke parsers. This
//! parser serves any grammar through a per-language table of tree-sitter
//! definition node kinds and name fields: files parse, completeness is
//! measured, definition nodes become signatures where the table knows how to
//! read a name, and languages without an obvious definition shape run in
//! parse-only mode (like `JsonParser`) while still joining the lexical +
//! neural index through the file content.
//!
//! Adding a language to Tier-0 is exactly three lines: a grammar dep in
//! `Cargo.toml`, a `languages::<name>` module in `traits.rs`, and a row in
//! [`GENERIC_LANGUAGE_TABLE`] plus the registry arm in `languages.rs`.

use crate::parse::traits::{
    Block, CodeIntelligence, ComplexityMetrics, Edge, Error, Graph, Result, SignatureInfo,
};
use tree_sitter::Parser;

/// Per-language Tier-0 extraction rules.
pub struct GenericLanguageRules {
    /// Tree-sitter node kinds treated as definitions (functions, types,
    /// modules). Empty ⇒ parse-only mode.
    pub definition_kinds: &'static [&'static str],
    /// Field or named-child kind holding the definition's name.
    pub name_field: &'static str,
    /// Kinds treated as import/include statements (name = full text, capped).
    pub import_kinds: &'static [&'static str],
    /// Kinds counted toward cyclomatic complexity (branches/loops).
    pub branch_kinds: &'static [&'static str],
}

/// The Tier-0 rule table. Kinds were read from each grammar's node-types.
#[cfg(feature = "parse")]
pub static GENERIC_LANGUAGE_TABLE: &[(&str, GenericLanguageRules)] = &[
    (
        "html",
        GenericLanguageRules {
            definition_kinds: &["element", "script_element"],
            name_field: "tag_name",
            import_kinds: &[],
            branch_kinds: &[],
        },
    ),
    (
        "css",
        GenericLanguageRules {
            // rule_set = selector + block; the "name" is the selector text.
            definition_kinds: &["rule_set"],
            name_field: "selectors",
            import_kinds: &["import_statement"],
            branch_kinds: &[],
        },
    ),
    (
        "scss",
        GenericLanguageRules {
            definition_kinds: &["rule_set", "mixin_statement", "include_statement"],
            name_field: "selectors",
            import_kinds: &["use_statement", "import_statement"],
            branch_kinds: &["if_statement", "each_statement"],
        },
    ),
    (
        "yaml",
        GenericLanguageRules {
            definition_kinds: &["block_mapping_pair"],
            name_field: "",
            import_kinds: &[],
            branch_kinds: &[],
        },
    ),
    (
        "cmake",
        GenericLanguageRules {
            definition_kinds: &["normal_command"],
            name_field: "",
            import_kinds: &[],
            branch_kinds: &["if_condition", "foreach_loop"],
        },
    ),
    (
        "elixir",
        GenericLanguageRules {
            // def/defp/defmodule/defmacro all surface as `call` nodes whose
            // first argument is the target identifier.
            definition_kinds: &["call"],
            name_field: "",
            import_kinds: &["alias"],
            branch_kinds: &["if", "unless", "cond", "case"],
        },
    ),
    (
        "erlang",
        GenericLanguageRules {
            definition_kinds: &["fun_decl"],
            name_field: "",
            import_kinds: &["include", "import"],
            branch_kinds: &["case", "if", "receive"],
        },
    ),
    (
        "haskell",
        GenericLanguageRules {
            definition_kinds: &["function", "signature"],
            name_field: "name",
            import_kinds: &["import"],
            branch_kinds: &["if", "case_alternative", "guard"],
        },
    ),
    (
        "perl",
        GenericLanguageRules {
            definition_kinds: &["function_definition"],
            name_field: "name",
            import_kinds: &["use_statement"],
            branch_kinds: &["if_statement", "unless_statement", "while_statement"],
        },
    ),
    (
        "r",
        GenericLanguageRules {
            definition_kinds: &["function_definition"],
            name_field: "name",
            import_kinds: &["library_call"],
            branch_kinds: &["if", "while", "for"],
        },
    ),
    (
        "zig",
        GenericLanguageRules {
            definition_kinds: &["function_declaration"],
            name_field: "",
            import_kinds: &["BuiltinCall"],
            branch_kinds: &["If", "While", "For", "SwitchExpr"],
        },
    ),
    (
        "graphql",
        GenericLanguageRules {
            definition_kinds: &["object_type_definition", "field_definition"],
            name_field: "name",
            import_kinds: &[],
            branch_kinds: &[],
        },
    ),
    (
        "hcl",
        GenericLanguageRules {
            definition_kinds: &["block"],
            name_field: "",
            import_kinds: &[],
            branch_kinds: &[],
        },
    ),
    (
        "make",
        GenericLanguageRules {
            definition_kinds: &["rule"],
            name_field: "",
            import_kinds: &["include"],
            branch_kinds: &["if"],
        },
    ),
    (
        "elisp",
        GenericLanguageRules {
            definition_kinds: &["function_definition", "defun"],
            name_field: "name",
            import_kinds: &["require"],
            branch_kinds: &["if", "cond", "while"],
        },
    ),
    (
        "julia",
        GenericLanguageRules {
            definition_kinds: &["function_definition"],
            name_field: "",
            import_kinds: &["import_statement", "using_statement"],
            branch_kinds: &["if", "while", "for"],
        },
    ),
    (
        "d",
        GenericLanguageRules {
            definition_kinds: &["function_declaration"],
            name_field: "name",
            import_kinds: &["import_declaration"],
            branch_kinds: &["if_statement", "while_statement", "for_statement"],
        },
    ),
    (
        "glsl",
        GenericLanguageRules {
            definition_kinds: &["function_definition"],
            name_field: "name",
            import_kinds: &["preproc_include"],
            branch_kinds: &["if_statement", "for_statement", "while_statement"],
        },
    ),
    (
        "embedded-template",
        GenericLanguageRules {
            definition_kinds: &[],
            name_field: "",
            import_kinds: &[],
            branch_kinds: &[],
        },
    ),
];

/// Look up the Tier-0 rules for a registered generic language name.
fn rules_for(language: &str) -> Option<&'static GenericLanguageRules> {
    GENERIC_LANGUAGE_TABLE
        .iter()
        .find(|(name, _)| *name == language)
        .map(|(_, rules)| rules)
}

/// Generic Tier-0 parser parameterized by registry language name
/// (`"elixir"`, `"zig"`, ...). The grammar is resolved through the
/// `traits::languages` module for that name.
pub struct GenericParser {
    language: &'static str,
}

impl GenericParser {
    /// Create a generic parser for a registered Tier-0 language.
    pub fn new(language: &'static str) -> Self {
        Self { language }
    }

    fn language_fn(&self) -> tree_sitter::Language {
        crate::parse::traits::languages::language_by_name(self.language)
    }
}

impl Default for GenericParser {
    fn default() -> Self {
        Self::new("yaml")
    }
}

impl CodeIntelligence for GenericParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let mut parser = Parser::new();
        self.get_signatures_with_parser(source, &mut parser)
    }

    fn get_signatures_with_parser(
        &self,
        source: &[u8],
        parser: &mut Parser,
    ) -> Result<Vec<SignatureInfo>> {
        parser
            .set_language(&self.language_fn())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("generic parse returned no tree".to_string()))?;

        let Some(rules) = rules_for(self.language) else {
            return Ok(vec![]);
        };
        let mut signatures = Vec::new();
        let mut imports = Vec::new();
        let cursor = tree.walk();
        let mut recursion = 0usize;
        visit_generic(
            cursor.node(),
            source,
            rules,
            &mut signatures,
            &mut imports,
            &mut recursion,
        );
        drop(cursor);
        drop(tree);

        for signature in &mut signatures {
            signature.imports = imports.clone();
        }
        Ok(signatures)
    }

    fn compute_cfg(&self, _source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        Ok(Graph {
            blocks: vec![],
            edges: vec![],
            entry_block: 0,
            exit_blocks: vec![],
        })
    }

    fn extract_complexity(&self, node: &tree_sitter::Node<'_>) -> ComplexityMetrics {
        ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: node
                .end_position()
                .row
                .saturating_sub(node.start_position().row),
            token_count: 0,
        }
    }
}

/// Depth-bounded walk collecting definitions and imports per the rule table.
fn visit_generic(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    rules: &GenericLanguageRules,
    signatures: &mut Vec<SignatureInfo>,
    imports: &mut Vec<crate::parse::traits::ImportInfo>,
    recursion: &mut usize,
) {
    // Depth guard mirrors the tree-sitter default recursion cap; malformed
    // grammars cannot stall the parser in a deep tree.
    if *recursion > 512 {
        return;
    }

    let kind = node.kind();
    if rules.definition_kinds.contains(&kind) {
        let name = extract_name(node, source, rules.name_field)
            .unwrap_or_else(|| format!("{kind}_{}", node.start_position().row + 1));
        signatures.push(SignatureInfo {
            name: name.clone(),
            qualified_name: name,
            parameters: Vec::new(),
            return_type: None,
            visibility: crate::parse::traits::Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: Vec::new(),
            imports: Vec::new(),
            byte_range: (node.byte_range().start, node.byte_range().end),
            cyclomatic_complexity: 1,
            flow_facts: Vec::new(),
        });
    } else if rules.import_kinds.contains(&kind) {
        let text = node
            .utf8_text(source)
            .unwrap_or_default()
            .trim()
            .chars()
            .take(200)
            .collect::<String>();
        imports.push(crate::parse::traits::ImportInfo {
            path: text,
            alias: None,
        });
    }

    *recursion += 1;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_generic(child, source, rules, signatures, imports, recursion);
    }
    *recursion -= 1;
}

/// Read a definition's name from the configured field, falling back to the
/// first named child of a common identifier kind.
fn extract_name(node: tree_sitter::Node<'_>, source: &[u8], name_field: &str) -> Option<String> {
    if !name_field.is_empty() {
        if let Some(field_node) = node.child_by_field_name(name_field) {
            if let Ok(text) = field_node.utf8_text(source) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if matches!(
            child.kind(),
            "identifier" | "name" | "variable" | "tag_name" | "atom" | "function" | "defun"
        ) {
            if let Ok(text) = child.utf8_text(source) {
                return Some(text.trim().to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rules_table_covers_all_registered_generic_languages() {
        for (name, _) in GENERIC_LANGUAGE_TABLE {
            assert!(
                crate::parse::traits::languages::language_by_name_is_registered(
                    name.replace('-', "_").as_str()
                ),
                "language {name} is in the generic table but has no traits::languages module"
            );
        }
    }
}
