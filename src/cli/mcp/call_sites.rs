//! Call-site extraction for `symbol_lookup` callers (brief §6.2).
//!
//! PDG `Call` edges are position-free `(caller, callee)` pairs — `Edge` and
//! `EdgeMetadata` carry no location, and the extractor deduplicates before
//! persisting, so a caller list alone cannot say *where* the call happens.
//! This module recovers the missing geometry on demand: the caller's own
//! `byte_range` is the enclosing function's span, so the file is parsed once
//! and the caller's subtree is searched for the call expression that names
//! the callee. The result is the one line an agent needs before editing:
//!
//! ```text
//! src/lib.rs:31    let foo = make_foo("a", "b");    (in fn use_make_foo)
//! ```
//!
//! Why tree-sitter and not text search: a text probe for `callee` matches the
//! caller's *definition* line, comments, strings and type positions. Matching
//! the AST node kinds (`call_expression`, `method_call_expression`,
//! `macro_invocation`, Python `call`) makes those false hits structurally
//! impossible. Language support: Rust and Python are fully wired; TypeScript
//! and JavaScript resolve plain, method and `new` calls. Other languages get
//! the caller metadata without a resolved line, which is disclosed, not
//! guessed.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::pdg::{Node, ProgramDependenceGraph};

/// Longest call-site line kept in an entry.
const MAX_SITE_LINE_CHARS: usize = 200;

/// One caller with (optionally) its resolved call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallerEntry {
    /// Caller symbol name — also the enclosing function of the call.
    pub name: String,
    /// File the caller lives in, as stored in the PDG.
    pub file: String,
    /// PDG node type as a string (`function`, `method`, …).
    pub kind: String,
    /// Ranked after non-test callers.
    pub is_test: bool,
    /// 1-based line of the call, when the language could be parsed.
    pub line: Option<usize>,
    /// The trimmed source line at [`CallerEntry::line`].
    pub site_line: Option<String>,
}

impl CallerEntry {
    /// JSON shape consumed by the renderer and `include_source` responses.
    pub(crate) fn to_json(&self) -> Value {
        let mut value = serde_json::json!({
            "name": self.name,
            "file": self.file,
            "type": self.kind,
            "is_test": self.is_test,
        });
        if let Some(line) = self.line {
            value["line"] = serde_json::json!(line);
        }
        if let Some(site) = &self.site_line {
            value["call_site"] = serde_json::json!(site);
        }
        value
    }

    /// Approximate response cost of this entry, used for budget filling.
    pub(crate) fn render_cost(&self) -> usize {
        self.name.len()
            + self.file.len()
            + self.kind.len()
            + 24
            + self.site_line.as_ref().map_or(0, |s| s.len() + 8)
    }
}

/// Parsed source for one file, cached across every caller in one response.
pub(crate) struct FileSource {
    /// The text the tree was parsed from (borrow discipline: both live here).
    pub text: String,
    /// `None` when the file's language has no LeIndex grammar.
    pub tree: Option<tree_sitter::Tree>,
}

/// Per-file cache shared by all callers of one lookup.
pub(crate) struct SourceCache {
    project_root: std::path::PathBuf,
    files: HashMap<String, Option<Arc<FileSource>>>,
}

impl SourceCache {
    pub(crate) fn new(project_root: &std::path::Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            files: HashMap::new(),
        }
    }

    /// Parsed source for the file a PDG node lives in, resolved against the
    /// project root (the same resolution `read_source_snippet_resolved` uses).
    pub(crate) fn file_source(&mut self, node: &Node) -> Option<Arc<FileSource>> {
        let path = node.file_path.to_string();
        let root = self.project_root.clone();
        let language = node.language.clone();
        let entry = self.files.entry(path).or_insert_with(|| {
            let resolved = match std::path::Path::new(node.file_path.as_ref()).is_absolute() {
                true => std::path::PathBuf::from(node.file_path.as_ref()),
                false => root.join(node.file_path.as_ref()),
            };
            let text = std::fs::read_to_string(resolved).ok()?;
            let ext = grammar_extension(&language)?;
            let id = crate::parse::grammar::LanguageId::from_extension(ext)?;
            let grammar = id.from_cache().ok()?;
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&grammar).ok()?;
            let tree = parser.parse(&text, None);
            Some(Arc::new(FileSource { text, tree }))
        });
        entry.as_ref().cloned()
    }
}

/// Tree-sitter extension for a PDG `language` value.
fn grammar_extension(language: &str) -> Option<&'static str> {
    match language.to_ascii_lowercase().as_str() {
        "rust" | "rs" => Some("rs"),
        "python" | "py" => Some("py"),
        "typescript" | "ts" => Some("ts"),
        "javascript" | "js" => Some("js"),
        _ => None,
    }
}

/// True when a symbol name follows the `test_*` / `*_test` conventions
/// (Rust/Python), plus the pytest/Jest descriptive names.
pub(crate) fn looks_like_test_name(name: &str) -> bool {
    let trimmed = name.trim_end_matches('\'');
    trimmed.starts_with("test_")
        || trimmed.ends_with("_test")
        || trimmed.starts_with("pytest_")
        || trimmed.starts_with("it_")
        || trimmed.starts_with("should_")
        || trimmed == "test"
        || trimmed == "it"
        || trimmed == "should"
}

/// True when a file path carries a test-file convention (Rust `*_test.rs`,
/// `tests/` dirs, Python `test_*.py`/`*_test.py`, Jest `*.test.*`/`*.spec.*`).
pub(crate) fn looks_like_test_file(path: &str) -> bool {
    let lowered = path.replace('\\', "/");
    let segments = lowered.split('/').collect::<Vec<_>>();
    if segments.contains(&"tests") || segments.contains(&"test") || segments.contains(&"__tests__")
    {
        return true;
    }
    let file = segments.last().copied().unwrap_or("");
    file.ends_with("_test.rs")
        || file.starts_with("test_")
        || file.ends_with("_test.py")
        || file.ends_with(".test.ts")
        || file.ends_with(".test.js")
        || file.ends_with(".test.tsx")
        || file.ends_with(".test.jsx")
        || file.ends_with(".spec.ts")
        || file.ends_with(".spec.js")
}

/// Caller is a test when its name, its file, or the attributes/decorators
/// directly above its definition say so. Attribute evidence needs the file
/// text (which the `include_source` path already loaded); without it the
/// name/path conventions still carry the ranking.
pub(crate) fn caller_is_test(node: &Node, file_text: Option<&str>) -> bool {
    if looks_like_test_name(&node.name) || looks_like_test_file(&node.file_path) {
        return true;
    }
    file_text.is_some_and(|text| attributes_mention_test(text, node.byte_range.0))
}

/// True when an attribute/decorator run directly above the byte range
/// mentions a test macro or marker (`#[test]`, `#[tokio::test]`,
/// `#[proptest]`, `@pytest.mark…`). Jest is name/path based, so it is covered
/// by [`looks_like_test_name`] / [`looks_like_test_file`].
fn attributes_mention_test(text: &str, fn_start: usize) -> bool {
    let before = &text[..fn_start.min(text.len())];
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("#[") {
            let lowered = trimmed.trim_start_matches('#').to_ascii_lowercase();
            let lowered = lowered.trim_start_matches('[');
            if lowered.starts_with("test")
                || lowered.starts_with("tokio::test")
                || lowered.starts_with("proptest")
                || lowered.contains("::test]")
                || lowered.contains("pytest.mark")
            {
                return true;
            }
            continue;
        }
        if trimmed.starts_with('@') {
            let lowered = trimmed.to_ascii_lowercase();
            if lowered.contains("pytest.mark") || lowered.contains("@test") {
                return true;
            }
            continue;
        }
        break;
    }
    false
}

/// The named node spanning exactly `range`, or the deepest named node that
/// contains its start. `range` is the PDG's own `byte_range`, so the exact
/// match is the common case; the fallback keeps extraction working when a
/// concurrent edit shifted the file since indexing.
fn node_at_range<'a>(
    root: tree_sitter::Node<'a>,
    range: (usize, usize),
) -> Option<tree_sitter::Node<'a>> {
    if root.start_byte() == range.0 && root.end_byte() == range.1 {
        return Some(root);
    }
    let mut best: Option<tree_sitter::Node<'a>> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.start_byte() > range.0 || node.end_byte() < range.0 {
            continue;
        }
        if node.is_named() {
            if node.start_byte() == range.0 && node.end_byte() == range.1 {
                return Some(node);
            }
            let smaller = best.is_none_or(|b| node.byte_range().len() < b.byte_range().len());
            if smaller {
                best = Some(node);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    best
}

/// Public wrapper for sibling modules (usages, tests_mode): the node spanning
/// exactly `range`, or the deepest node containing its start.
pub(crate) fn node_for_range(
    root: tree_sitter::Node<'_>,
    range: (usize, usize),
) -> Option<tree_sitter::Node<'_>> {
    node_at_range(root, range)
}

/// Item kinds whose calls belong to a nested definition, per language. A hit
/// inside one is attributed to the nested item (which carries its own PDG
/// node), never to the outer function. TypeScript arrow functions are
/// deliberately absent: they have no PDG node of their own, so their calls
/// stay attributed to the enclosing function.
fn nested_item_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" | "rs" => &["function_item"],
        "python" | "py" => &["function_definition", "lambda"],
        _ => &[],
    }
}

/// Call-expression node kinds per language, with the field holding the
/// callee text.
fn call_kinds(language: &str) -> &'static [(&'static str, &'static str)] {
    match language {
        "rust" | "rs" => &[
            ("call_expression", "function"),
            ("method_call_expression", "method"),
            ("macro_invocation", "macro"),
        ],
        "python" | "py" => &[("call", "function")],
        "typescript" | "ts" | "javascript" | "js" => &[
            ("call_expression", "function"),
            ("new_expression", "constructor"),
        ],
        _ => &[],
    }
}

/// A resolved call site: 1-based line plus the trimmed line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallSite {
    pub line: usize,
    pub text: String,
}

/// Search the caller's own subtree for a call naming `callee`. Returns the
/// first hit in source order. The comparison is on the callee's last path
/// segment (`Vec::push` matches a `push` method call), anchored to a complete
/// segment so `push_meta` never matches `push`.
pub(crate) fn find_call_in_fn(
    fn_node: tree_sitter::Node<'_>,
    text: &str,
    language: &str,
    callee: &str,
) -> Option<CallSite> {
    let kinds = call_kinds(language);
    if kinds.is_empty() {
        return None;
    }
    let skip = nested_item_kinds(language);
    let callee_last = callee_last_segment(callee);

    let mut stack = vec![fn_node];
    let mut first_hit: Option<(usize, usize)> = None;
    while let Some(node) = stack.pop() {
        if node != fn_node && skip.contains(&node.kind()) {
            continue;
        }
        for (kind, field) in kinds {
            if node.kind() != *kind {
                continue;
            }
            let Some(callee_child) = node.child_by_field_name(field) else {
                continue;
            };
            let Ok(callee_text) = callee_child.utf8_text(text.as_bytes()) else {
                continue;
            };
            if names_match(callee_text, callee, &callee_last)
                && first_hit.is_none_or(|(start, _)| node.start_byte() < start)
            {
                first_hit = Some((node.start_byte(), node.end_byte()));
            }
        }
        // A call made inside a macro body exists only as a token sequence:
        // `assert_eq!(callee(1), 2)` has no call_expression node. Paths are
        // reassembled from ident/`::`/ident token runs, and a path whose last
        // segment names the callee followed by `(` is the site.
        if language.starts_with("rust")
            && node.kind() == "macro_invocation"
            && let Some(token_tree) = node
                .children(&mut node.walk())
                .find(|child| child.kind() == "token_tree")
        {
            let mut flat: Vec<tree_sitter::Node<'_>> = Vec::new();
            let mut walk = vec![token_tree];
            while let Some(token) = walk.pop() {
                let mut cursor = token.walk();
                let children: Vec<_> = token.children(&mut cursor).collect();
                if children.is_empty() {
                    flat.push(token);
                }
                for child in children.into_iter().rev() {
                    walk.push(child);
                }
            }
            flat.sort_by_key(|token| token.start_byte());
            let mut path: Vec<String> = Vec::new();
            let mut path_start: Option<usize> = None;
            let mut after_sep = false;
            for (idx, token) in flat.iter().enumerate() {
                let kind = token.kind();
                let token_text = token.utf8_text(text.as_bytes()).unwrap_or("");
                let is_ident = kind == "identifier" || kind == "type_identifier";
                let is_sep = token_text == "::";
                let next_is_open_paren = flat
                    .get(idx + 1)
                    .is_some_and(|next| next.utf8_text(text.as_bytes()) == Ok("("));
                if is_ident {
                    if !after_sep && !path.is_empty() {
                        // Two identifiers without a separator: the previous
                        // path ended (`x y` starts a fresh path at `y`).
                        path.clear();
                        path_start = None;
                    }
                    if path.is_empty() {
                        path_start = Some(token.start_byte());
                    }
                    path.push(token_text.to_string());
                    after_sep = false;
                } else if is_sep && !path.is_empty() {
                    // Keep the path open across the separator.
                    after_sep = true;
                } else {
                    path.clear();
                    path_start = None;
                    after_sep = false;
                }
                // The call itself: path + `(`. Comparing on the last segment
                // so `Vec::push` matches a `push` callee from the PDG.
                let last_seg = path.last().map(String::as_str);
                if next_is_open_paren
                    && let Some(last_seg) = last_seg
                    && last_seg.eq_ignore_ascii_case(&callee_last)
                    && let Some(start) = path_start
                    && first_hit.is_none_or(|(hit_start, _)| start < hit_start)
                {
                    first_hit = Some((start, token.end_byte()));
                }
            }
        }
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    let (start, _) = first_hit?;
    Some(CallSite {
        line: byte_to_line(text, start),
        text: line_at(text, start),
    })
}

/// Last `::`/`.`-separated segment of a callee path, macro `!` stripped.
fn callee_last_segment(callee: &str) -> String {
    let path = callee.rsplit("::").next().unwrap_or(callee);
    path.rsplit('.')
        .next()
        .unwrap_or(path)
        .trim_end_matches('!')
        .to_string()
}

/// `callee_text` names `callee` when the last segments agree and the text is
/// either exactly the callee or a suffix/ prefix path of it.
fn names_match(callee_text: &str, callee: &str, callee_last: &str) -> bool {
    let text = callee_text.trim().trim_end_matches('!');
    if text.is_empty() {
        return false;
    }
    if !callee_last_segment(text).eq_ignore_ascii_case(callee_last) {
        return false;
    }
    let callee = callee.trim_end_matches('!');
    text == callee || callee.ends_with(text) || text.ends_with(callee)
}

/// 1-based line number of a byte offset.
pub(crate) fn byte_to_line(text: &str, byte: usize) -> usize {
    let bound = byte.min(text.len());
    1 + text.as_bytes()[..bound]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// The trimmed line containing `byte`, capped to [`MAX_SITE_LINE_CHARS`].
pub(crate) fn line_at(text: &str, byte: usize) -> String {
    let bound = byte.min(text.len());
    let before = text[..bound].rsplit('\n').next().unwrap_or("");
    let rest = text[bound..].split('\n').next().unwrap_or("");
    let full = format!("{before}{rest}").trim().to_string();
    if full.chars().count() > MAX_SITE_LINE_CHARS {
        let mut cut: String = full.chars().take(MAX_SITE_LINE_CHARS).collect();
        cut.push('…');
        cut
    } else {
        full
    }
}

/// Resolve and rank every direct caller of `node_id`: call-site line plus
/// test classification per caller, non-test callers first (stable within
/// each class). Files are read and parsed through `cache`, so twenty callers
/// in one file cost one read and one parse. `callee_name` is the looked-up
/// symbol's name; it anchors the site search (the PDG's Call edges carry no
/// position, so the subtree search is what recovers the line).
pub(crate) fn caller_entries(
    pdg: &ProgramDependenceGraph,
    node_id: crate::graph::pdg::NodeId,
    resolve_sites: bool,
    callee_name: &str,
    cache: &mut SourceCache,
) -> Vec<CallerEntry> {
    let mut entries: Vec<CallerEntry> = Vec::new();
    for caller_id in crate::cli::mcp::helpers::get_direct_callers(pdg, node_id) {
        let Some(node) = pdg.get_node(caller_id) else {
            continue;
        };
        let mut entry = CallerEntry {
            name: node.name.clone(),
            file: node.file_path.to_string(),
            kind: crate::cli::mcp::helpers::node_type_str(&node.node_type).to_string(),
            is_test: false,
            line: None,
            site_line: None,
        };
        if resolve_sites {
            let source = cache.file_source(node);
            entry.is_test = caller_is_test(node, source.as_ref().map(|src| src.text.as_str()));
            if let Some(source) = source
                && let Some(tree) = &source.tree
            {
                let fn_node = node_at_range(tree.root_node(), node.byte_range);
                if let Some(fn_node) = fn_node {
                    let language = node.language.as_str();
                    if let Some(site) =
                        find_call_in_fn(fn_node, &source.text, language, callee_name)
                    {
                        entry.line = Some(site.line);
                        entry.site_line = Some(site.text);
                    }
                }
            }
        } else {
            entry.is_test = caller_is_test(node, None);
        }
        entries.push(entry);
    }
    rank_test_last(&mut entries);
    entries
}

/// Stable partition: non-test callers keep their edge order ahead of tests.
pub(crate) fn rank_test_last(entries: &mut [CallerEntry]) {
    entries.sort_by_key(|entry| entry.is_test);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn leaf_fn(x: u32) -> u32 {\n    x + 1\n}\n\nfn caller_fn() -> u32 {\n    let a = leaf_fn(1);\n    let b = leaf_fn(2) * 3;\n    a + b\n}\n";

    fn fn_node_in<'a>(root: tree_sitter::Node<'a>, src: &str, name: &str) -> tree_sitter::Node<'a> {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_item" {
                let found = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src.as_bytes()).ok());
                if found == Some(name) {
                    return node;
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        panic!("fn {name} not found in fixture");
    }

    fn parse_rust(src: &'_ str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(
                &crate::parse::grammar::LanguageId::from_extension("rs")
                    .unwrap()
                    .from_cache()
                    .unwrap(),
            )
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn test_call_site_is_the_calling_line_not_the_definition() {
        let tree = parse_rust(SRC);
        let fn_node = fn_node_in(tree.root_node(), SRC, "caller_fn");
        let site = find_call_in_fn(fn_node, SRC, "rust", "leaf_fn").expect("call site found");
        // leaf_fn(1) on line 6 comes first in source order (1-based).
        assert_eq!(site.line, 6, "first call in source order: {:?}", site.text);
        assert_eq!(site.text, "let a = leaf_fn(1);");
    }

    #[test]
    fn test_macro_internal_call_site_is_found() {
        let src =
            "fn callee(x: u32) -> u32 { x }\n\nfn tester() {\n    assert_eq!(callee(1), 2);\n}\n";
        let tree = parse_rust(src);
        let fn_node = fn_node_in(tree.root_node(), src, "tester");
        let site = find_call_in_fn(fn_node, src, "rust", "callee").expect("macro call site");
        assert_eq!(site.line, 4);
        assert_eq!(site.text, "assert_eq!(callee(1), 2);");
    }

    #[test]
    fn test_python_call_site_is_found() {
        let src = "def leaf(x):\n    return x\n\ndef use_leaf():\n    y = leaf(3)\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(
                &crate::parse::grammar::LanguageId::from_extension("py")
                    .unwrap()
                    .from_cache()
                    .unwrap(),
            )
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let mut fn_node = None;
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_definition" {
                let found = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(src.as_bytes()).ok());
                if found == Some("use_leaf") {
                    fn_node = Some(node);
                    break;
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
        let site =
            find_call_in_fn(fn_node.expect("use_leaf"), src, "python", "leaf").expect("py site");
        assert_eq!(site.line, 5);
        assert_eq!(site.text, "y = leaf(3)");
    }

    #[test]
    fn test_names_match_is_segment_anchored() {
        let last = callee_last_segment("leaf_fn");
        assert!(names_match("leaf_fn", "leaf_fn", &last));
        assert!(names_match("module::leaf_fn", "leaf_fn", &last));
        assert!(!names_match("not_leaf_fn", "leaf_fn", &last));
        let push = callee_last_segment("Vec::push");
        assert!(names_match("push", "Vec::push", &push));
        assert!(!names_match("push_meta", "Vec::push", &push));
        // A macro callee strips its bang on both sides.
        let bang = callee_last_segment("assert_eq!");
        assert!(names_match("assert_eq", "assert_eq!", &bang));
    }

    #[test]
    fn test_ranking_puts_non_test_callers_first() {
        let mut entries = vec![
            CallerEntry {
                name: "test_helper".into(),
                file: "a.rs".into(),
                kind: "function".into(),
                is_test: true,
                line: None,
                site_line: None,
            },
            CallerEntry {
                name: "prod_user".into(),
                file: "b.rs".into(),
                kind: "function".into(),
                is_test: false,
                line: None,
                site_line: None,
            },
            CallerEntry {
                name: "test_other".into(),
                file: "c.rs".into(),
                kind: "function".into(),
                is_test: true,
                line: None,
                site_line: None,
            },
        ];
        rank_test_last(&mut entries);
        assert_eq!(entries[0].name, "prod_user");
        assert_eq!(entries[1].name, "test_helper");
        assert_eq!(entries[2].name, "test_other");
    }

    #[test]
    fn test_test_conventions_by_name_and_path() {
        assert!(looks_like_test_name("test_leaf_direct"));
        assert!(looks_like_test_name("parse_edge_test"));
        assert!(looks_like_test_name("it_round_trips"));
        assert!(!looks_like_test_name("protest_marcha"));
        assert!(looks_like_test_file("tests/api_test.rs"));
        assert!(looks_like_test_file("src/parser_test.rs"));
        assert!(looks_like_test_file("src/test_utils.py"));
        assert!(looks_like_test_file("web/app.test.ts"));
        assert!(looks_like_test_file("web/app.spec.js"));
        assert!(!looks_like_test_file("src/lib.rs"));
    }

    #[test]
    fn test_attributes_mark_test_fns() {
        let src = "#[test]\nfn test_a() {}\n\n#[tokio::test]\nasync fn test_b() {}\n\n#[cfg(feature = \"x\")]\nfn prod() {}\n";
        assert!(attributes_mention_test(src, src.find("fn test_a").unwrap()));
        assert!(attributes_mention_test(
            src,
            src.find("async fn test_b").unwrap()
        ));
        assert!(!attributes_mention_test(src, src.find("fn prod").unwrap()));
    }

    #[test]
    fn test_line_helpers() {
        let text = "alpha\nbeta\ngamma";
        assert_eq!(byte_to_line(text, 0), 1);
        assert_eq!(byte_to_line(text, 6), 2);
        assert_eq!(line_at(text, 6), "beta");
        assert_eq!(line_at(text, 0), "alpha");
    }
}
