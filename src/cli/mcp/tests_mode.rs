//! Test locator for `symbol_lookup` `kind=tests` (brief §6.4).
//!
//! Answers "which tests exercise this symbol?" from the call graph — the
//! caller set, transitively and bounded (direct callers first) — and for each
//! test the *exact* command that runs it, so no `find` + read + filter
//! guessing is needed:
//!
//! - Rust unit tests (inside `src/`, under any module nesting):
//!   `cargo test -p <crate> --lib <module::path>::<fn> -- --exact`
//! - Rust integration tests (`tests/*.rs`):
//!   `cargo test -p <crate> --test <file_stem> <fn>`
//! - pytest / Jest: `<runner> <path>::<fn>` / `<runner> <path> -t <fn>`
//!
//! The module path is derived by walking the file's syntax tree from the
//! item to the root and collecting the `mod` names on the way, which gets
//! `mod.rs`, nested `mod tests` and `#[path = "..."]` declarations right
//! without a filesystem walk. The `-p` crate name comes from the nearest
//! ancestor `Cargo.toml`.

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

use crate::graph::pdg::{Node, ProgramDependenceGraph};

/// Upper bound on transitive caller levels.
const MAX_CALL_DEPTH: usize = 5;

/// Hard cap on reported test targets; overflow is disclosed, not silent.
pub(crate) const MAX_TEST_TARGETS: usize = 50;

/// A test that exercises the target symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestTarget {
    /// Test function name.
    pub name: String,
    /// File the test lives in, as stored in the PDG.
    pub file: String,
    /// PDG node type string (`function`, `method`).
    pub kind: String,
    /// 1-based definition line, when the file could be parsed.
    pub line: Option<usize>,
    /// Call-graph distance: 1 = direct caller of the symbol.
    pub call_depth: usize,
    /// What marked it as a test (`#[test]`, `name convention`, `test file`).
    pub evidence: &'static str,
    /// Exact runnable command.
    pub command: String,
}

impl TestTarget {
    /// JSON shape for the response payload.
    pub(crate) fn to_json(&self) -> Value {
        let mut value = serde_json::json!({
            "name": self.name,
            "file": self.file,
            "type": self.kind,
            "call_depth": self.call_depth,
            "evidence": self.evidence,
            "command": self.command,
        });
        if let Some(line) = self.line {
            value["line"] = serde_json::json!(line);
        }
        value
    }

    /// Approximate response cost, used for budget filling.
    pub(crate) fn render_cost(&self) -> usize {
        self.name.len() + self.file.len() + self.command.len() + 48
    }
}

/// The test framework a target runs under, inferred from path and language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestRunner {
    /// Rust unit test inside `src/`.
    CargoUnit,
    /// Rust integration test under `tests/`.
    CargoIntegration,
    /// Python `pytest`.
    Pytest,
    /// JavaScript `jest` / `vitest`.
    Jest,
}

fn runner_for(path: &str, language: &str) -> Option<TestRunner> {
    let lowered = path.replace('\\', "/");
    let segments: Vec<&str> = lowered.split('/').collect();
    let file = segments.last().copied().unwrap_or("");
    let under_tests = segments.len() >= 2 && segments[segments.len() - 2] == "tests";
    match language {
        "rust" | "rs" => {
            if under_tests && file.ends_with(".rs") {
                Some(TestRunner::CargoIntegration)
            } else if !under_tests && file.ends_with(".rs") {
                Some(TestRunner::CargoUnit)
            } else {
                None
            }
        }
        "python" | "py" => Some(TestRunner::Pytest),
        "typescript" | "ts" | "javascript" | "js" => Some(TestRunner::Jest),
        _ => None,
    }
}

/// The `-p <crate>` value: the nearest ancestor `Cargo.toml`'s package name.
///
/// Bounded walk (16 levels) so a stray test file above the workspace cannot
/// spin here; without a manifest the command simply omits `-p`.
fn cargo_package(file: &str) -> Option<String> {
    let mut dir = Path::new(file).parent()?.to_path_buf();
    for _ in 0..16 {
        let manifest = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            let mut in_package = false;
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('[') {
                    in_package = trimmed == "[package]";
                    continue;
                }
                if in_package && let Some(rest) = trimmed.strip_prefix("name") {
                    let rest = rest.trim_start().strip_prefix('=')?.trim();
                    let name = rest.trim_matches('"');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// The `module::path::to::fn` string cargo expects for a unit test.
///
/// Two things contribute to the path:
///
/// 1. the OUT-OF-LINE ancestry: for `src/foo.rs`, the enclosing `foo` module
///    is declared in `src/lib.rs` (`mod foo;`), not in the file itself. It is
///    recovered by walking the crate root's `mod` declarations (see
///    [`out_of_line_module_ancestry`]) until the chain reaches this file.
/// 2. the INLINE ancestry inside the file: from the function item to the file
///    root, every enclosing `mod_item` (including `#[path]` variants).
///
/// Without (1) the filter for `src/foo.rs`'s `tests::test_x` came out as
/// `tests::test_x`; `--exact` then matched nothing and cargo exited 0 after
/// running zero tests, so a failing test read as a pass.
fn unit_module_path(
    source: &crate::cli::mcp::call_sites::FileSource,
    node: &Node,
    fn_name: &str,
) -> Option<String> {
    let tree = source.tree.as_ref()?;
    let fn_node = crate::cli::mcp::call_sites::node_for_range(tree.root_node(), node.byte_range)?;
    // Inline ancestors, collected nearest-first then reversed to root order.
    let mut inline: Vec<String> = Vec::new();
    let mut cursor = fn_node;
    while let Some(parent) = cursor.parent() {
        if parent.kind() == "mod_item"
            && let Some(name) = parent.child_by_field_name("name")
            && let Ok(name) = name.utf8_text(source.text.as_bytes())
        {
            inline.push(name.to_string());
        }
        cursor = parent;
    }
    inline.reverse();
    let mut modules = out_of_line_module_ancestry(&node.file_path).unwrap_or_default();
    modules.extend(inline);
    modules.push(fn_name.to_string());
    Some(modules.join("::"))
}

/// Module path of the out-of-line module whose file IS `file`, by resolving
/// `mod` declarations from the crate root with the compiler's rules.
///
/// A `mod x;` declared in a file is looked up relative to the declaring
/// module's directory, which is the file's own directory for a root or
/// `mod.rs`, and `<dir>/<stem>/` for any other file (so `src/foo.rs`'s
/// children live in `src/foo/`). An inline `mod a { mod b; }` makes `a` part
/// of that directory. `#[path = "p"]` is relative to the same directory.
///
/// Only the chain that actually reaches `file` is returned; an empty chain
/// means `file` IS the crate root. The walk is bounded (depth 32, 4,000
/// declarations) so a pathological tree cannot spin.
fn out_of_line_module_ancestry(file: &str) -> Option<Vec<String>> {
    use std::path::{Path, PathBuf};

    fn parse(text: &str) -> Option<tree_sitter::Tree> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::parse::traits::languages::rust::language())
            .ok()?;
        parser.parse(text, None)
    }

    /// `#[path = "…"]` on a `mod_item`, if present.
    ///
    /// In the tree-sitter Rust grammar an attribute is a SIBLING
    /// `attribute_item` immediately before the item it decorates, not a
    /// child of it, so look at the previous sibling.
    fn path_attribute(item: tree_sitter::Node<'_>, text: &str) -> Option<String> {
        let mut prev = item.prev_sibling();
        while let Some(node) = prev {
            if node.kind() != "attribute_item" {
                break;
            }
            let rendered = node.utf8_text(text.as_bytes()).ok()?;
            if let Some(value) = rendered
                .trim_start_matches("#[")
                .trim_end_matches(']')
                .trim()
                .strip_prefix("path")
                .and_then(|rest| rest.trim().strip_prefix('='))
                .map(|v| v.trim().trim_matches('"'))
                && !value.is_empty()
            {
                return Some(value.to_string());
            }
            prev = node.prev_sibling();
        }
        None
    }

    /// The directory that `mod x;` declared in (`file`, `is_mod_rs`) with an
    /// inline-nesting of `inline` resolves against.
    fn module_dir(file: &Path, is_mod_rs: bool, inline: &[String]) -> PathBuf {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut base = if is_mod_rs {
            dir
        } else {
            let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            dir.join(stem)
        };
        for segment in inline {
            base = base.join(segment);
        }
        base
    }

    /// Where `mod x;` (with optional `#[path]`) lands, or `None` when neither
    /// candidate exists on disk.
    fn resolve(base: &Path, name: &str, attr: Option<&str>) -> Option<PathBuf> {
        if let Some(rel) = attr {
            let p = base.join(rel);
            return p.is_file().then_some(p);
        }
        let flat = base.join(format!("{name}.rs"));
        if flat.is_file() {
            return Some(flat);
        }
        let nested = base.join(name).join("mod.rs");
        nested.is_file().then_some(nested)
    }

    let target = Path::new(file);
    let canonical_target = target.canonicalize().ok()?;
    let mut package_dir = target.parent()?.to_path_buf();
    for _ in 0..16 {
        if package_dir.join("Cargo.toml").is_file() {
            break;
        }
        if !package_dir.pop() {
            return None;
        }
    }
    let root = ["lib.rs", "main.rs"]
        .into_iter()
        .map(|r| package_dir.join("src").join(r))
        .find(|p| p.is_file())?;
    let same = |p: &Path| p.canonicalize().ok() == Some(canonical_target.clone());
    if same(&root) {
        return Some(Vec::new());
    }

    // Files still to expand, with the module path that declares them.
    let mut queue = vec![(root, true, Vec::<String>::new())];
    let mut visited = 0usize;
    while let Some((file, is_mod_rs, prefix)) = queue.pop() {
        if prefix.len() > 32 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let Some(tree) = parse(&text) else {
            continue;
        };
        // Walk every `mod_item` at any depth, tracking inline nesting so the
        // resolution directory is right for children of inline modules.
        let mut walk = vec![(tree.root_node(), Vec::<String>::new(), prefix.clone())];
        while let Some((node, inline, path)) = walk.pop() {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "mod_item" {
                    if child.child_count() > 0 {
                        walk.push((child, inline.clone(), path.clone()));
                    }
                    continue;
                }
                visited += 1;
                if visited > 4_000 {
                    return None;
                }
                let Some(name) = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                else {
                    continue;
                };
                let mut path = path.clone();
                path.push(name.to_string());
                let has_body = child
                    .children(&mut child.walk())
                    .any(|c| c.kind() == "declaration_list");
                if has_body {
                    // Inline module: descend with it added to the nesting.
                    let mut inline = inline.clone();
                    inline.push(name.to_string());
                    walk.push((child, inline, path));
                    continue;
                }
                let base = module_dir(&file, is_mod_rs, &inline);
                let attr = path_attribute(child, &text);
                let Some(resolved) = resolve(&base, name, attr.as_deref()) else {
                    continue;
                };
                if same(&resolved) {
                    return Some(path);
                }
                let child_is_mod_rs =
                    resolved.file_name().and_then(|n| n.to_str()) == Some("mod.rs");
                queue.push((resolved, child_is_mod_rs, path));
            }
        }
    }
    None
}

/// The exact command that runs one test.
fn command_for(
    node: &Node,
    line: Option<usize>,
    source: Option<&crate::cli::mcp::call_sites::FileSource>,
) -> String {
    let file = node.file_path.as_ref();
    let dash_p = cargo_package(file)
        .map(|crate_name| format!("-p {crate_name} "))
        .unwrap_or_default();
    match runner_for(file, &node.language) {
        Some(TestRunner::CargoIntegration) => {
            let stem = Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            format!("cargo test {dash_p}--test {stem} {}", node.name)
        }
        Some(TestRunner::CargoUnit) => {
            let path = source
                .and_then(|source| unit_module_path(source, node, &node.name))
                .unwrap_or_else(|| node.name.clone());
            format!("cargo test {dash_p}--lib {path} -- --exact")
        }
        Some(TestRunner::Pytest) => {
            let name = &node.name;
            let locator = match line {
                Some(line) => format!("{file}::{name} or {file}:{line}"),
                None => format!("{file}::{name}"),
            };
            format!("pytest {locator}")
        }
        Some(TestRunner::Jest) => format!("npx jest {} -t {}", file, node.name),
        None => {
            let at = line.map_or_else(|| "?".to_string(), |l| l.to_string());
            format!(
                "(no runner known for {} files) read {}:{}",
                node.language, file, at
            )
        }
    }
}

/// Callers of `node_id` by BFS level: direct callers at depth 1 first, then
/// their callers, bounded. Deduplicated globally — the shortest path wins.
fn callers_by_depth(
    pdg: &ProgramDependenceGraph,
    node_id: crate::graph::pdg::NodeId,
    max_depth: usize,
) -> Vec<(crate::graph::pdg::NodeId, usize)> {
    let max_depth = max_depth.clamp(1, MAX_CALL_DEPTH);
    let mut visited = HashSet::new();
    let mut out = Vec::new();
    let mut frontier = vec![node_id];
    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for &front in &frontier {
            for caller in crate::cli::mcp::helpers::get_direct_callers(pdg, front) {
                // Recursion (a self-edge) must not make a fn its own caller.
                if caller != node_id && visited.insert(caller) {
                    out.push((caller, depth));
                    next.push(caller);
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    out
}

/// Whether a `#[test]`-family attribute sits directly above the item.
///
/// Reads backwards from the item's start byte across attribute/comment lines
/// until real code appears. `#[tokio::test]`, `#[proptest]` and multi-line
/// attributes are all matched; anything else stops the scan.
fn attributes_say_test(text: &str, fn_start: usize) -> Option<&'static str> {
    let before = &text[..fn_start.min(text.len())];
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lowered = trimmed.to_ascii_lowercase();
        if lowered.starts_with("#[tokio::test") || lowered.starts_with("#[test") {
            return Some("#[test]");
        }
        if lowered.starts_with("#[proptest") {
            return Some("proptest");
        }
        if lowered.starts_with("#[") {
            // Another attribute (cfg, allow, …) — keep scanning upwards.
            continue;
        }
        if lowered.starts_with("//") || lowered.starts_with("/*") {
            continue;
        }
        break;
    }
    None
}

/// Classify a caller as a test and say what marked it.
fn classify_test(node: &Node, file_text: Option<&str>) -> Option<&'static str> {
    if let Some(evidence) = file_text.and_then(|text| attributes_say_test(text, node.byte_range.0))
    {
        return Some(evidence);
    }
    let name = node.name.as_str();
    if name.starts_with("test_") || name.ends_with("_test") {
        return Some("name convention");
    }
    let path = node.file_path.replace('\\', "/");
    let segments: Vec<&str> = path.split('/').collect();
    let file = segments.last().copied().unwrap_or("");
    if file.ends_with("_test.rs")
        || file.starts_with("test_")
        || file.ends_with("_test.py")
        || file.ends_with(".test.ts")
        || file.ends_with(".test.js")
        || file.ends_with(".test.tsx")
        || file.ends_with(".test.jsx")
        || file.ends_with(".spec.ts")
        || file.ends_with(".spec.js")
    {
        return Some("test file");
    }
    if segments.contains(&"tests") || segments.contains(&"test") || segments.contains(&"__tests__")
    {
        return Some("tests dir");
    }
    let pytest_jest = matches!(
        node.language.as_str(),
        "python" | "py" | "typescript" | "ts" | "javascript" | "js"
    ) && (name.starts_with("test_")
        || name.starts_with("pytest_")
        || name == "test"
        || name == "it"
        || name == "should");
    if pytest_jest {
        return Some("pytest/jest name");
    }
    None
}

/// Every test exercising `node_id`, direct callers first, bounded.
///
/// Returns the targets plus `truncated`, set when a test was found but
/// withheld because `max_targets` was exhausted — the caller reports it.
pub(crate) fn tests_for(
    pdg: &ProgramDependenceGraph,
    node_id: crate::graph::pdg::NodeId,
    depth: usize,
    cache: &mut crate::cli::mcp::call_sites::SourceCache,
    max_targets: usize,
) -> (Vec<TestTarget>, bool) {
    let mut targets = Vec::new();
    let mut truncated = false;
    for (caller_id, call_depth) in callers_by_depth(pdg, node_id, depth) {
        if targets.len() >= max_targets {
            truncated = true;
            break;
        }
        let Some(node) = pdg.get_node(caller_id) else {
            continue;
        };
        let source = cache.file_source(node);
        let Some(evidence) = classify_test(node, source.as_ref().map(|src| src.text.as_str()))
        else {
            continue;
        };
        let line = source.as_ref().and_then(|src| {
            src.tree.as_ref().and_then(|tree| {
                crate::cli::mcp::call_sites::node_for_range(tree.root_node(), node.byte_range).map(
                    |fn_node| {
                        1 + src.text.as_bytes()[..fn_node.start_byte()]
                            .iter()
                            .filter(|b| **b == b'\n')
                            .count()
                    },
                )
            })
        });
        let command = command_for(node, line, source.as_deref());
        targets.push(TestTarget {
            name: node.name.clone(),
            file: node.file_path.to_string(),
            kind: crate::cli::mcp::helpers::node_type_str(&node.node_type).to_string(),
            line,
            call_depth,
            evidence,
            command,
        });
    }
    (targets, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, file: &str, language: &str, byte_range: (usize, usize)) -> Node {
        Node {
            id: format!("{file}::{name}"),
            node_type: crate::graph::pdg::NodeType::Function,
            name: name.to_string(),
            file_path: std::sync::Arc::from(file),
            byte_range,
            complexity: 1,
            language: language.to_string(),
        }
    }

    /// A real tempdir manifest, so `cargo_package`'s nearest-ancestor walk has
    /// something true to find — the command must carry the real crate name.
    fn manifest_dir(name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .expect("manifest");
        dir
    }

    fn parse_rust(source: &str) -> crate::cli::mcp::call_sites::FileSource {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(
                &crate::parse::grammar::LanguageId::from_extension("rs")
                    .unwrap()
                    .from_cache()
                    .unwrap(),
            )
            .unwrap();
        crate::cli::mcp::call_sites::FileSource {
            text: source.to_string(),
            tree: parser.parse(source, None),
        }
    }

    #[test]
    fn test_unit_test_command_is_exact_with_module_path() {
        let dir = manifest_dir("proj");
        let source = "mod inner {\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn test_x() {\n            assert!(true);\n        }\n    }\n}\n";
        let src = parse_rust(source);
        let byte_range = (source.find("fn test_x").unwrap(), source.len());
        let test_fn = node(
            "test_x",
            &dir.path().join("src/inner.rs").display().to_string(),
            "rust",
            byte_range,
        );
        let cmd = command_for(&test_fn, Some(5), Some(&src));
        assert_eq!(
            cmd, "cargo test -p proj --lib inner::tests::test_x -- --exact",
            "command: {cmd}"
        );
    }

    /// A real crate on disk whose unit tests live in out-of-line modules.
    fn crate_with_out_of_line_tests() -> tempfile::TempDir {
        let dir = manifest_dir("outline");
        let src = dir.path().join("src");
        std::fs::write(
            src.join("lib.rs"),
            "pub mod foo;\npub mod nested;\n#[path = \"renamed.rs\"]\npub mod aliased;\n",
        )
        .unwrap();
        // src/foo.rs declares a child of its own -> src/foo/child.rs
        std::fs::write(
            src.join("foo.rs"),
            "mod child;\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_foo() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
        std::fs::create_dir_all(src.join("foo")).unwrap();
        std::fs::write(
            src.join("foo").join("child.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_child() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
        // src/nested/mod.rs style
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(
            src.join("nested").join("mod.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_modrs() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            src.join("renamed.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_aliased() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
        // A root-level test, and an inline module inside the root.
        std::fs::write(
            src.join("lib.rs"),
            "pub mod foo;\npub mod nested;\n#[path = \"renamed.rs\"]\npub mod aliased;\n\nmod inline_root {\n    mod tests {\n        #[test]\n        fn test_in_inline_root() {\n            assert!(true);\n        }\n    }\n}\n#[test]\nfn test_at_root() {\n    assert!(true);\n}\n",
        )
        .unwrap();
        dir
    }

    fn command_for_test_in(dir: &tempfile::TempDir, rel: &str, name: &str) -> String {
        let file = dir.path().join("src").join(rel);
        let source = std::fs::read_to_string(&file).unwrap();
        let src = parse_rust(&source);
        let start = source.find(&format!("fn {name}")).unwrap();
        let test_fn = node(
            name,
            &file.display().to_string(),
            "rust",
            (start, source.len()),
        );
        command_for(&test_fn, Some(1), Some(&src))
    }

    /// For a test in `src/foo.rs`, the enclosing `foo` module is declared in
    /// `src/lib.rs`, not in the file. The filter used to come out as
    /// `tests::test_x`; `--exact` then matched nothing and cargo exited 0
    /// after running zero tests, so a failing test read as a pass.
    #[test]
    fn test_unit_test_filter_includes_out_of_line_module_ancestry() {
        let dir = crate_with_out_of_line_tests();
        assert_eq!(
            command_for_test_in(&dir, "foo.rs", "test_in_foo"),
            "cargo test -p outline --lib foo::tests::test_in_foo -- --exact"
        );
        // Child of a non-mod-rs file: `src/foo.rs` -> `src/foo/child.rs`.
        assert_eq!(
            command_for_test_in(&dir, "foo/child.rs", "test_in_child"),
            "cargo test -p outline --lib foo::child::tests::test_in_child -- --exact"
        );
        // `mod.rs` style.
        assert_eq!(
            command_for_test_in(&dir, "nested/mod.rs", "test_in_modrs"),
            "cargo test -p outline --lib nested::tests::test_in_modrs -- --exact"
        );
        // `#[path = "…"]` alias keeps the declared name, not the file name.
        assert_eq!(
            command_for_test_in(&dir, "renamed.rs", "test_in_aliased"),
            "cargo test -p outline --lib aliased::tests::test_in_aliased -- --exact"
        );
        // Root-level and inline-in-root tests are unchanged.
        assert_eq!(
            command_for_test_in(&dir, "lib.rs", "test_at_root"),
            "cargo test -p outline --lib test_at_root -- --exact"
        );
        assert_eq!(
            command_for_test_in(&dir, "lib.rs", "test_in_inline_root"),
            "cargo test -p outline --lib inline_root::tests::test_in_inline_root -- --exact"
        );
    }

    /// The filter must actually run the test: `--exact` with a wrong path
    /// runs zero tests and still exits 0. Run the generated command against
    /// real cargo on a FAILING test and require a non-zero exit.
    #[test]
    fn test_generated_exact_filter_runs_the_test_under_real_cargo() {
        let dir = crate_with_out_of_line_tests();
        // Make the out-of-line test fail.
        let foo = dir.path().join("src/foo.rs");
        let text = std::fs::read_to_string(&foo)
            .unwrap()
            .replace("assert!(true)", "assert!(false)");
        std::fs::write(&foo, text).unwrap();
        let command = command_for_test_in(&dir, "foo.rs", "test_in_foo");
        assert!(command.starts_with("cargo test -p outline"), "{command}");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(dir.path())
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .expect("cargo available");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("test foo::tests::test_in_foo ... FAILED"),
            "the test must actually run: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success(), "a failing test must fail the run");
    }

    #[test]
    fn test_root_level_unit_test_has_no_module_prefix() {
        let dir = manifest_dir("proj");
        let source = "#[test]\nfn test_root() {\n    assert!(true);\n}\n";
        let src = parse_rust(source);
        let byte_range = (source.find("fn test_root").unwrap(), source.len());
        let test_fn = node(
            "test_root",
            &dir.path().join("src/lib.rs").display().to_string(),
            "rust",
            byte_range,
        );
        let cmd = command_for(&test_fn, Some(2), Some(&src));
        assert_eq!(
            cmd, "cargo test -p proj --lib test_root -- --exact",
            "command: {cmd}"
        );
    }

    #[test]
    fn test_integration_test_command_uses_test_target() {
        let dir = manifest_dir("proj");
        let test_fn = node(
            "test_api",
            &dir.path().join("tests/api_test.rs").display().to_string(),
            "rust",
            (0, 1),
        );
        let cmd = command_for(&test_fn, None, None);
        assert_eq!(
            cmd, "cargo test -p proj --test api_test test_api",
            "cmd: {cmd}"
        );
    }

    #[test]
    fn test_pytest_and_jest_commands() {
        let py = node(
            "test_round_trip",
            "/proj/tests/core_test.py",
            "python",
            (0, 1),
        );
        assert_eq!(
            command_for(&py, Some(12), None),
            "pytest /proj/tests/core_test.py::test_round_trip or /proj/tests/core_test.py:12"
        );
        let js = node(
            "should round trip",
            "/proj/src/x.test.js",
            "javascript",
            (0, 1),
        );
        assert_eq!(
            command_for(&js, None, None),
            "npx jest /proj/src/x.test.js -t should round trip"
        );
    }

    #[test]
    fn test_classify_prefers_attributes_then_names_then_paths() {
        let src = "#[test]\nfn test_a() {}\n\n#[cfg(unix)]\nfn helper() {}\n";
        let attr_fn = node(
            "irrelevant_name",
            "/proj/src/lib.rs",
            "rust",
            (src.find("fn helper").unwrap(), src.len()),
        );
        // Attribute above helper is #[cfg(unix)] — not a test...
        assert_eq!(classify_test(&attr_fn, Some(src)), None);
        let attr_test = node(
            "irrelevant_name",
            "/proj/src/lib.rs",
            "rust",
            (src.find("fn test_a").unwrap(), src.len()),
        );
        assert_eq!(classify_test(&attr_test, Some(src)), Some("#[test]"));
        // Name convention without attributes.
        let named = node("test_only_name", "/proj/src/lib.rs", "rust", (0, 1));
        assert_eq!(classify_test(&named, Some("")), Some("name convention"));
        // Test-file path convention.
        let in_file = node("verify", "/proj/tests/api_test.rs", "rust", (0, 1));
        assert_eq!(classify_test(&in_file, Some("")), Some("test file"));
        // Non-test production fn with no evidence.
        let prod = node("compute", "/proj/src/lib.rs", "rust", (0, 1));
        assert_eq!(classify_test(&prod, Some("")), None);
    }

    #[test]
    fn test_callers_by_depth_orders_direct_first_and_dedups() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let mk = |pdg: &mut crate::graph::pdg::ProgramDependenceGraph, name: &str| {
            pdg.add_node(node(name, "/p/src/lib.rs", "rust", (0, 1)))
        };
        let leaf = mk(&mut pdg, "leaf");
        let mid = mk(&mut pdg, "mid");
        let top = mk(&mut pdg, "top");
        pdg.add_call_edges(vec![(mid, leaf), (top, mid), (leaf, leaf)]);
        let by_depth = callers_by_depth(&pdg, leaf, 3);
        assert_eq!(by_depth.len(), 2, "self-loop deduped: {by_depth:?}");
        assert_eq!(by_depth[0].1, 1);
        assert_eq!(by_depth[1].1, 2);
    }

    #[test]
    fn test_cargo_package_reads_nearest_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("crates/sub");
        std::fs::create_dir_all(&nested).expect("dirs");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture_pkg\"\n",
        )
        .unwrap();
        let file = nested.join("src/lib.rs");
        let name = cargo_package(file.to_str().expect("utf8 path"));
        assert_eq!(name.as_deref(), Some("fixture_pkg"));
        // A path with no manifest above it yields None.
        assert_eq!(cargo_package("/definitely/not/a/crate/x.rs"), None);
    }
}
