//! Construct and match usage sites (brief §6.3).
//!
//! "Who constructs `Foo { .. }`?" and "who matches on `Shade::Dark`?" must be
//! answered from the syntax tree, not from text: a text probe hits comments,
//! strings, and every type-position mention (`fn f(foo: Foo)`). This module
//! walks tree-sitter ASTs — the same grammars the structural search uses —
//! and only reports nodes whose *kind* is a construction or pattern position:
//!
//! | language | construct | match |
//! |---|---|---|
//! | Rust | `struct_expression` | `match_pattern` (match arms), `let_condition` (`if let`), `let_declaration` patterns, `matches!` bodies |
//! | TypeScript/JavaScript | `new_expression` | `switch_case` |
//! | Python | — (no typed struct literals) | pattern nodes inside `match_statement` |
//!
//! `Foo::new(..)` is deliberately NOT a construct site: it is a call, and the
//! call graph already reports it (it reaches this tool as a caller). A site
//! inside a macro token tree (`matches!`) is matched at token level, which is
//! the deepest structure the grammar has there; string/char/comment tokens
//! are excluded so `"Shade::Dark"` cannot masquerade as a pattern.
//!
//! A bare type query (`Shade`) is the whole-enum form: it matches variant
//! patterns whose type segment is `Shade` (`Shade::Dark`). A qualified query
//! (`Shade::Dark`) matches that variant only. Or-patterns
//! (`A | B`) count when ANY alternative names the target.
//!
//! The enclosing function is resolved from a prepass over function-like
//! definitions in the same file, so every site renders as
//! `path:line  code  (in fn name)`.

/// One usage site found in a file (path and budget fill added by the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageSite {
    /// 1-based line of the site.
    pub line: usize,
    /// The trimmed source line at the site.
    pub site_line: String,
    /// Enclosing function name, when the site sits inside one.
    pub enclosing: Option<String>,
    /// Syntax shape: `struct literal`, `match arm`, `if let`, `let pattern`,
    /// `matches!`, `new`, `switch case`, `match case`.
    pub form: &'static str,
}

/// What to look for, resolved from the symbol name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageQuery {
    /// `construct` or `match` (§6.3).
    pub kind: UsageKind,
    /// The symbol's last path segment (`Foo`, `Dark`).
    pub last: String,
    /// The full given path when it carried one (`core::Shade::Dark`).
    pub full: Option<String>,
}

/// The §6.3 usage kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageKind {
    /// Struct-literal / `new` construction sites.
    Construct,
    /// Enum-variant pattern sites.
    Match,
}

impl UsageQuery {
    /// Build a query from the looked-up symbol name.
    pub(crate) fn from_symbol(kind: UsageKind, symbol: &str) -> Self {
        let trimmed = symbol.trim();
        let last = trimmed
            .rsplit("::")
            .next()
            .unwrap_or(trimmed)
            .rsplit('.')
            .next()
            .unwrap_or(trimmed)
            .to_string();
        let full = if trimmed.contains("::") || trimmed.contains('.') {
            Some(trimmed.to_string())
        } else {
            None
        };
        Self { kind, last, full }
    }

    /// True when this is the whole-enum form (a bare type name).
    fn is_whole_type(&self) -> bool {
        self.full.is_none()
    }
}

/// Split a candidate into `|` alternatives and normalize `::` to `.`.
fn alternatives(candidate: &str) -> impl Iterator<Item = &str> {
    candidate
        .split('|')
        .map(str::trim)
        .filter(|alt| !alt.is_empty())
}

/// Names the target: exact, full-path, or last-segment match on a pattern
/// text, all anchored to complete path segments.
fn names_target(candidate: &str, query: &UsageQuery) -> bool {
    let normalize = |s: &str| s.replace("::", ".");
    let last = normalize(&query.last);
    let full = query.full.as_ref().map(|full| {
        normalize(full.trim())
            .split('.')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    alternatives(candidate).any(|alt| {
        // A leading `::` is a crate-root absolute path, never a relative
        // enum path; filtering empty segments below would erase the anomaly.
        if alt.starts_with("::") {
            return false;
        }
        let cand = normalize(alt);
        let segments: Vec<&str> = cand.split('.').filter(|s| !s.is_empty()).collect();
        let Some(terminal) = segments.last() else {
            return false;
        };
        let base = terminal == &last
            || (query.is_whole_type()
                && segments.len() >= 2
                && segments[segments.len() - 2] == last);
        if !base {
            return false;
        }
        // Pattern evidence has two honesty tiers. A struct-literal name
        // (`Foo { .. }`) or a full pattern node (`Shade::Dark`) carries its
        // own path, so a bare last segment is fine. But a bare identifier
        // token from inside a `matches!` token tree proves nothing about the
        // enum, so whole-type and bare-token combinations are rejected —
        // except when the query itself is qualified, where the token is all
        // the evidence the grammar can give and the suffix rule below is the
        // disclosed approximation.
        if query.kind == UsageKind::Match && query.is_whole_type() && segments.len() == 1 {
            return false;
        }
        match &full {
            Some(full) => {
                let full_seg: Vec<&str> = full.iter().map(String::as_str).collect();
                (segments == full_seg[..] || full_seg.ends_with(&segments[..]))
                    && segments.len() <= full.len()
            }
            None => true,
        }
    })
}

/// Function-like definition kinds per language, with the field naming them.
fn enclosing_fns(
    root: tree_sitter::Node<'_>,
    text: &str,
    language: &str,
) -> Vec<(usize, usize, String)> {
    let mut table = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let named = match language {
            "rust" | "rs" => Some(("function_item", "name")),
            "python" | "py" => Some(("function_definition", "name")),
            "typescript" | "ts" | "javascript" | "js" => Some(("function_declaration", "name")),
            _ => None,
        };
        if let Some((kind, field)) = named
            && node.kind() == kind
            && let Some(name_node) = node.child_by_field_name(field)
            && let Ok(name) = name_node.utf8_text(text.as_bytes())
        {
            table.push((node.start_byte(), node.end_byte(), name.to_string()));
            // Nested functions are separate entries; do not descend.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    table.sort_unstable_by_key(|(start, _, _)| *start);
    table
}

/// Innermost enclosing function name for a byte offset.
fn enclosing_fn_at(table: &[(usize, usize, String)], byte: usize) -> Option<String> {
    table
        .iter()
        .rev()
        .find(|&&(start, end, _)| start <= byte && byte < end)
        .map(|(_, _, name)| name.clone())
}

/// True for tokens that must never count as pattern evidence inside a macro
/// token tree: literals and comments, not identifiers.
fn token_tree_noise(kind: &str) -> bool {
    matches!(
        kind,
        "string_literal" | "char_literal" | "line_comment" | "block_comment"
    )
}

/// Collect construct sites (`kind=construct`) in one parsed file.
fn collect_construct(
    root: tree_sitter::Node<'_>,
    text: &str,
    language: &str,
    query: &UsageQuery,
) -> Vec<(usize, &'static str)> {
    let mut sites = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match language {
            "rust" | "rs" => {
                if node.kind() == "struct_expression"
                    && let Some(name_node) = node.child_by_field_name("name")
                    && let Ok(name) = name_node.utf8_text(text.as_bytes())
                    && names_target(name, query)
                {
                    sites.push((node.start_byte(), "struct literal"));
                }
            }
            "typescript" | "ts" | "javascript" | "js" if node.kind() == "new_expression" => {
                // The constructor is the constructor's first named child;
                // TS grammars disagree on the field name, so take the
                // type node directly.
                let ctor = node.child_by_field_name("constructor").or_else(|| {
                    node.children(&mut node.walk())
                        .find(|child| child.is_named())
                });
                if let Some(ctor) = ctor
                    && let Ok(name) = ctor.utf8_text(text.as_bytes())
                    && names_target(name, query)
                {
                    sites.push((node.start_byte(), "new"));
                }
            }
            // Python has no typed struct-literal construction; dict literals
            // are untyped and reporting them would be a guess.
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    sites
}

/// One candidate pattern: its byte offset, its exact pattern text, and the
/// syntax form to label it with.
struct PatternHit {
    byte: usize,
    pattern_text: String,
    form: &'static str,
}

/// Collect match-pattern candidates in one parsed file.
fn collect_match_candidates(
    root: tree_sitter::Node<'_>,
    text: &str,
    language: &str,
) -> Vec<PatternHit> {
    // Returns Some(hit) instead of pushing into a borrowed closure vec: the
    // descent below both builds hits this way and pushes directly.
    fn node_hit(node: tree_sitter::Node<'_>, text: &str, form: &'static str) -> Option<PatternHit> {
        let pattern_text = node.utf8_text(text.as_bytes()).ok()?;
        Some(PatternHit {
            byte: node.start_byte(),
            pattern_text: pattern_text.to_string(),
            form,
        })
    }
    let mut hits = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match language {
            "rust" | "rs" => match node.kind() {
                "match_pattern" => {
                    if let Some(hit) = node_hit(node, text, "match arm") {
                        hits.push(hit);
                    }
                }
                // `if let PAT = expr` parses as a `let_condition` inside the
                // if_expression's condition field; its `pattern` field is the
                // pattern alone.
                "let_condition" => {
                    if let Some(pattern) = node.child_by_field_name("pattern")
                        && let Some(hit) = node_hit(pattern, text, "if let")
                    {
                        hits.push(hit);
                    }
                }
                "let_declaration" => {
                    if let Some(pattern) = node.child_by_field_name("pattern")
                        && let Some(hit) = node_hit(pattern, text, "let pattern")
                    {
                        hits.push(hit);
                    }
                }
                "macro_invocation" => {
                    let is_matches = node
                        .child_by_field_name("macro")
                        .and_then(|macro_node| macro_node.utf8_text(text.as_bytes()).ok())
                        .is_some_and(|name| name.trim_end_matches('!') == "matches");
                    if is_matches
                        && let Some(token_tree) = node
                            .children(&mut node.walk())
                            .find(|child| child.kind() == "token_tree")
                    {
                        // Token-tree contents are flat tokens: `Shade::Light`
                        // arrives as ident, `::`, ident. Reassemble maximal
                        // `ident (:: ident)*` sequences so whole-enum queries
                        // can qualify them and a bare variant token cannot.
                        let mut flat: Vec<tree_sitter::Node<'_>> = Vec::new();
                        let mut walk = vec![token_tree];
                        while let Some(token) = walk.pop() {
                            if token_tree_noise(token.kind()) {
                                continue;
                            }
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
                        let flush =
                            |path: &mut Vec<String>,
                             start: &mut Option<usize>,
                             hits: &mut Vec<PatternHit>| {
                                if path.len() >= 2
                                    && let Some(start) = *start
                                {
                                    hits.push(PatternHit {
                                        byte: start,
                                        pattern_text: path.join("::"),
                                        form: "matches!",
                                    });
                                }
                                path.clear();
                                *start = None;
                            };
                        for token in flat {
                            let kind = token.kind();
                            let is_ident = kind == "identifier" || kind == "type_identifier";
                            let is_sep = token.utf8_text(text.as_bytes()) == Ok("::");
                            if is_ident {
                                if !after_sep && !path.is_empty() {
                                    // Two identifiers without a separator: the
                                    // previous path ended (`x y` starts a fresh
                                    // path at `y`; `s,` already flushed at the
                                    // comma).
                                    flush(&mut path, &mut path_start, &mut hits);
                                }
                                if path.is_empty() {
                                    path_start = Some(token.start_byte());
                                }
                                if let Ok(text) = token.utf8_text(text.as_bytes()) {
                                    path.push(text.to_string());
                                }
                                after_sep = false;
                            } else if is_sep && !path.is_empty() {
                                // Keep the path open across the separator.
                                after_sep = true;
                            } else {
                                flush(&mut path, &mut path_start, &mut hits);
                                after_sep = false;
                            }
                        }
                        flush(&mut path, &mut path_start, &mut hits);
                    }
                }
                _ => {}
            },
            "python" | "py" => {
                // The pattern kinds are unnamed and carry no field pointing
                // at themselves; their kind names only exist inside a
                // match_statement, so keying on the kind is exact.
                const PY_PATTERN_KINDS: [&str; 9] = [
                    "pattern",
                    "pattern_list",
                    "as_pattern",
                    "union_pattern",
                    "case_pattern",
                    "class_pattern",
                    "dict_pattern",
                    "list_pattern",
                    "tuple_pattern",
                ];
                if PY_PATTERN_KINDS.contains(&node.kind())
                    && let Some(hit) = node_hit(node, text, "match case")
                {
                    hits.push(hit);
                }
            }
            "typescript" | "ts" | "javascript" | "js" => {
                if node.kind() == "switch_case"
                    && let Ok(clause) = node.utf8_text(text.as_bytes())
                    && let Some(case_idx) = clause.find("case")
                {
                    let after = &clause[case_idx + "case".len()..];
                    let expr = after.trim_start();
                    let expr_text = expr.split(':').next().unwrap_or("").trim().to_string();
                    if !expr_text.is_empty() {
                        hits.push(PatternHit {
                            // Point at the case expression, not the clause.
                            byte: node.start_byte() + (clause.len() - expr.len()),
                            pattern_text: expr_text,
                            form: "switch case",
                        });
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    hits
}

/// Every matching usage site in one file, in source order.
pub(crate) fn collect_sites(
    root: tree_sitter::Node<'_>,
    text: &str,
    language: &str,
    query: &UsageQuery,
) -> Vec<UsageSite> {
    let fn_table = enclosing_fns(root, text, language);
    let raw: Vec<(usize, &'static str)> = match query.kind {
        UsageKind::Construct => collect_construct(root, text, language, query),
        UsageKind::Match => collect_match_candidates(root, text, language)
            .into_iter()
            .filter(|hit| names_target(&hit.pattern_text, query))
            .map(|hit| (hit.byte, hit.form))
            .collect(),
    };
    let mut sites: Vec<UsageSite> = raw
        .into_iter()
        .map(|(byte, form)| UsageSite {
            line: crate::cli::mcp::call_sites::byte_to_line(text, byte),
            site_line: crate::cli::mcp::call_sites::line_at(text, byte),
            enclosing: enclosing_fn_at(&fn_table, byte),
            form,
        })
        .collect();
    sites.sort_by_key(|site| site.line);
    sites.dedup_by(|a, b| a.line == b.line && a.form == b.form);
    sites
}

/// Directories whose sources are vendored or generated, never first-party
/// usage sites worth reporting. Hidden directories (`.git`, `.leindex`, …)
/// are skipped by name prefix in [`source_files`].
const SKIP_DIRS: [&str; 6] = [
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "__pycache__",
];

/// Hard cap on files walked for one usage query (fail-safe against
/// pathological trees); overflow is disclosed via `more`, not silent.
const MAX_USAGE_FILES: usize = 4000;

/// Hard cap on reported sites per query.
const MAX_USAGE_SITES: usize = 500;

/// One usage site with its project-relative file and language, ready for the
/// §6.3 response body.
pub(crate) struct ProjectSite {
    /// Path relative to the project root (absolute when the file sits above it).
    pub file: String,
    /// Language name (`rust`, `python`, `typescript`, `javascript`).
    pub language: &'static str,
    /// The site itself.
    pub site: UsageSite,
}

/// Language name for a source file extension. `tsx`/`jsx` are deliberately
/// absent: no grammar is wired for them, and guessing would misreport sites.
fn language_for_path(path: &std::path::Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" => Some("typescript"),
        "js" => Some("javascript"),
        _ => None,
    }
}

/// Parseable first-party source files under `root`, sorted for deterministic
/// order, capped at `limit`. Hidden and vendored/build directories are
/// skipped.
fn source_files(root: &std::path::Path, limit: usize) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if files.len() >= limit {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if files.len() >= limit {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if language_for_path(&path).is_some() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Scan every parseable first-party source under the project for `query`'s
/// usage sites. Files are parsed once through the shared cache; results are
/// in deterministic (path, line) order. The third element is `true` when the
/// site cap hid further hits, so callers can disclose truncation honestly.
pub(crate) fn sites_in_project(
    project_root: &std::path::Path,
    query: &UsageQuery,
) -> (Vec<ProjectSite>, usize, bool) {
    let mut cache = crate::cli::mcp::call_sites::SourceCache::new(project_root);
    let mut sites: Vec<ProjectSite> = Vec::new();
    let mut scanned = 0usize;
    let mut more = false;
    for path in source_files(project_root, MAX_USAGE_FILES) {
        let Some(language) = language_for_path(&path) else {
            continue;
        };
        let Some(path_text) = path.to_str() else {
            continue;
        };
        // A file-summary node is only a cache key here: `file_source` uses its
        // path and language to read + parse, nothing else.
        let summary = crate::graph::pdg::Node::new_file_summary(path_text, language);
        let Some(source) = cache.file_source(&summary) else {
            continue;
        };
        let Some(tree) = source.tree.as_ref() else {
            continue;
        };
        scanned += 1;
        let rel = path
            .strip_prefix(project_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        for site in collect_sites(tree.root_node(), &source.text, language, query) {
            if sites.len() >= MAX_USAGE_SITES {
                more = true;
                break;
            }
            sites.push(ProjectSite {
                file: rel.clone(),
                language,
                site,
            });
        }
        if more {
            break;
        }
    }
    (sites, scanned, more)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str, ext: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(
                &crate::parse::grammar::LanguageId::from_extension(ext)
                    .expect("grammar")
                    .from_cache()
                    .expect("cached grammar"),
            )
            .expect("set language");
        parser.parse(src, None).expect("parse")
    }

    const RUST_SRC: &str = r#"//! Doc comment mentioning Foo { name: 1 } must never be a site.
pub struct Foo {
    pub name: String,
}

pub enum Shade {
    Light,
    Dark,
}

pub fn make_foo() -> Foo {
    let _comment = 0; // Foo { name: 0 } in a comment
    let _text = "Foo { name: 1 }";
    Foo {
        name: String::new(),
    }
}

pub fn ctor_call() -> Foo {
    Foo::new()
}

pub fn use_shade(s: Shade) -> &'static str {
    match s {
        Shade::Light => "bright",
        Shade::Dark => "dim",
    }
}

pub fn dark_check(s: &Shade) -> bool {
    if let Shade::Dark = s {
        return true;
    }
    matches!(s, Shade::Light)
}
"#;

    fn query(kind: UsageKind, symbol: &str) -> UsageQuery {
        UsageQuery::from_symbol(kind, symbol)
    }

    #[test]
    fn test_construct_sites_exclude_comments_strings_and_ctor_calls() {
        let tree = parse(RUST_SRC, "rs");
        let sites = collect_sites(
            tree.root_node(),
            RUST_SRC,
            "rust",
            &query(UsageKind::Construct, "Foo"),
        );
        // Exactly one site: the literal inside make_foo (line 14). Foo::new()
        // in ctor_call is a call, not a construction; the doc comment, the
        // line comment and the string literal are not struct_expressions.
        assert_eq!(sites.len(), 1, "sites: {sites:?}");
        assert_eq!(sites[0].line, 14);
        assert_eq!(sites[0].form, "struct literal");
        assert_eq!(sites[0].enclosing.as_deref(), Some("make_foo"));
        assert!(sites[0].site_line.contains("Foo {"));
    }

    #[test]
    fn test_construct_type_positions_are_not_sites() {
        let tree = parse(RUST_SRC, "rs");
        let sites = collect_sites(
            tree.root_node(),
            RUST_SRC,
            "rust",
            &query(UsageKind::Construct, "Foo"),
        );
        assert!(sites.iter().all(
            |site| !site.site_line.contains("-> Foo") && !site.site_line.contains("pub struct")
        ));
    }

    #[test]
    fn test_match_sites_find_match_arm_if_let_and_matches() {
        let tree = parse(RUST_SRC, "rs");
        let sites = collect_sites(
            tree.root_node(),
            RUST_SRC,
            "rust",
            &query(UsageKind::Match, "Shade::Dark"),
        );
        let forms: Vec<&str> = sites.iter().map(|site| site.form).collect();
        // Dark appears as a match arm (use_shade) and an if let (dark_check).
        assert!(forms.contains(&"match arm"), "forms: {forms:?}");
        assert!(forms.contains(&"if let"), "forms: {forms:?}");
        assert_eq!(sites.len(), 2, "sites: {sites:?}");
        assert!(sites.iter().all(|site| site.site_line.contains("Dark")));
    }

    #[test]
    fn test_matches_macro_body_surfaces_token_level_sites() {
        let tree = parse(RUST_SRC, "rs");
        let sites = collect_sites(
            tree.root_node(),
            RUST_SRC,
            "rust",
            &query(UsageKind::Match, "Shade::Light"),
        );
        let forms: Vec<&str> = sites.iter().map(|site| site.form).collect();
        assert!(forms.contains(&"match arm"), "forms: {forms:?}");
        assert!(forms.contains(&"matches!"), "forms: {forms:?}");
        let matches_site = sites
            .iter()
            .find(|site| site.form == "matches!")
            .expect("matches! site");
        assert_eq!(matches_site.enclosing.as_deref(), Some("dark_check"));
        // No reported site may START with a string literal (the "bright"
        // literal must not become a pseudo-site); the Dark sites' lines are
        // expression lines that legitimately contain quotes.
        assert!(
            sites
                .iter()
                .all(|site| !site.site_line.trim_start().starts_with('"'))
        );
    }

    #[test]
    fn test_whole_enum_query_matches_variant_type_segment() {
        let tree = parse(RUST_SRC, "rs");
        let sites = collect_sites(
            tree.root_node(),
            RUST_SRC,
            "rust",
            &query(UsageKind::Match, "Shade"),
        );
        // Whole-enum form: every `Shade::Variant` pattern qualifies. Fixture
        // lines (1-based): match arms 25/26, if let 31, matches! 34. The
        // `Foo::new()` lines never appear: a call is not a pattern site.
        let lines: Vec<usize> = sites.iter().map(|site| site.line).collect();
        assert!(lines.contains(&25), "match arm lines: {sites:?}");
        assert!(lines.contains(&26), "match arm lines: {sites:?}");
        assert!(lines.contains(&31), "if let lines: {sites:?}");
        assert!(lines.contains(&34), "matches! lines: {sites:?}");
    }

    #[test]
    fn test_or_patterns_match_any_alternative() {
        // No or-pattern in the fixture; assert the matcher directly.
        let q = query(UsageKind::Match, "Shade::Dark");
        assert!(names_target("Shade::Dark | Shade::Light", &q));
        assert!(names_target("Shade::Light | Shade::Dark", &q));
        assert!(!names_target("Shade::Light", &q));
    }

    #[test]
    fn test_let_pattern_sites() {
        let src = "pub enum E { A }\nfn check(e: &E) -> bool {\n    if let E::A = e { true } else { false }\n}\nfn bind(e: E) {\n    let E::A = e else { return };\n}\n";
        let tree = parse(src, "rs");
        let sites = collect_sites(
            tree.root_node(),
            src,
            "rust",
            &query(UsageKind::Match, "E::A"),
        );
        let forms: Vec<&str> = sites.iter().map(|site| site.form).collect();
        assert!(forms.contains(&"if let"), "forms: {forms:?}");
        assert!(forms.contains(&"let pattern"), "forms: {forms:?}");
    }

    #[test]
    fn test_python_match_case_sites() {
        let src = "def use_shade(s):\n    match s:\n        case Shade.Dark:\n            return \"dim\"\n";
        let tree = parse(src, "py");
        let sites = collect_sites(
            tree.root_node(),
            src,
            "python",
            &query(UsageKind::Match, "Shade::Dark"),
        );
        assert_eq!(sites.len(), 1, "sites: {sites:?}");
        assert_eq!(sites[0].line, 3);
        assert_eq!(sites[0].form, "match case");
        assert_eq!(sites[0].enclosing.as_deref(), Some("use_shade"));
    }

    #[test]
    fn test_typescript_new_and_switch_sites() {
        let src = "class Foo {}\nfunction build(): Foo {\n    return new Foo();\n}\nfunction pick(shape: Shape): string {\n    switch (shape) {\n        case Shape.Circle:\n            return \"circle\";\n    }\n}\n";
        let tree = parse(src, "ts");
        let construct = collect_sites(
            tree.root_node(),
            src,
            "typescript",
            &query(UsageKind::Construct, "Foo"),
        );
        assert_eq!(construct.len(), 1, "construct: {construct:?}");
        assert_eq!(construct[0].form, "new");
        let match_sites = collect_sites(
            tree.root_node(),
            src,
            "typescript",
            &query(UsageKind::Match, "Shape::Circle"),
        );
        assert_eq!(match_sites.len(), 1, "match: {match_sites:?}");
        assert_eq!(match_sites[0].form, "switch case");
        assert_eq!(match_sites[0].line, 7);
        assert_eq!(match_sites[0].enclosing.as_deref(), Some("pick"));
    }

    #[test]
    fn test_python_construct_is_honestly_unsupported() {
        let src = "def build():\n    return Foo(name=\"x\")\n";
        let tree = parse(src, "py");
        let sites = collect_sites(
            tree.root_node(),
            src,
            "python",
            &query(UsageKind::Construct, "Foo"),
        );
        // A Python constructor call is a call edge, not a literal site; the
        // empty answer is the honest one for `kind=construct`.
        assert!(sites.is_empty());
    }

    #[test]
    fn test_names_target_is_segment_anchored() {
        let q = query(UsageKind::Match, "Shade::Dark");
        assert!(names_target("Shade::Dark", &q));
        assert!(!names_target("DarkMatter", &q));
        assert!(!names_target("::Dark", &q));
        let q2 = query(UsageKind::Match, "Shade");
        // Whole-enum: qualified variants qualify, foreign types do not.
        assert!(names_target("Shade::Dark", &q2));
        assert!(!names_target("Tint::Dark", &q2));
    }
}
