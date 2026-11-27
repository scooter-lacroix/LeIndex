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
use std::path::{Path, PathBuf};

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

/// The name of a `[[test]]` target the package manifest DECLARES for `file`,
/// if any.
///
/// Cargo lets a manifest rename an integration target:
///
/// ```toml
/// [[test]]
/// name = "acceptance"
/// path = "tests/api_test.rs"
/// ```
///
/// That builds a target called `acceptance`, and `--test api_test` — the
/// filename stem — is then a name Cargo rejects with `no test target named
/// api_test`, listing `acceptance` as available. Only the declared name runs
/// the tests, so it is resolved BEFORE any filename inference. A `[[test]]`
/// without a `path` defaults to `tests/<name>.rs`, exactly like Cargo.
///
/// Paths are compared canonically, so a `./tests/api_test.rs` spelling in the
/// manifest matches the absolute path the PDG stores.
fn declared_integration_target(file: &str) -> Option<String> {
    let package_dir = cargo_package_dir(Path::new(file))?;
    let text = std::fs::read_to_string(package_dir.join("Cargo.toml")).ok()?;
    let table = text.parse::<toml::Table>().ok()?;
    let canonical = Path::new(file).canonicalize().ok()?;
    table
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(toml::Value::as_str)?;
            let declared = match entry.get("path").and_then(toml::Value::as_str) {
                Some(path) => PathBuf::from(path),
                None => Path::new("tests").join(format!("{name}.rs")),
            };
            let full = package_dir.join(declared);
            (full.canonicalize().ok() == Some(canonical.clone())).then(|| name.to_string())
        })
        .next()
}

/// The Cargo integration-target name for a file under `tests/`, when exactly
/// one target can be PROVEN to compile it.
///
/// Cargo has no per-file ownership: a file is compiled into every target
/// whose module tree reaches it. That was verified against real cargo
/// (round 11, 4697090278) rather than assumed — with `tests/api/main.rs`
/// declaring `mod shared_test;` and `tests/other_test.rs` importing the same
/// file via `#[path]`, BOTH targets compiled it and BOTH ran its tests.
///
/// So the claim `--test <dir>` is only safe when the directory's `main.rs`
/// genuinely declares THIS file as a module. Two things can make that false:
///
/// * `tests/<dir>/main.rs` does not exist — the directory is not a target at
///   all (Codex round 8, id 4691102595);
/// * `main.rs` exists but never `mod`s this file — it is a shared helper that
///   some OTHER target imports, so `--test <dir>` selects a target that does
///   not contain the test.
///
/// In both cases `None` is the honest answer, and the caller falls back to
/// `--tests` + a name filter, which runs the test wherever it was compiled in
/// instead of naming a target that may not have it.
///
/// The filter is deliberately a SUBSTRING: a test in a module registers as
/// `<module>::<fn>`, so `-- --exact` on the bare fn name matches nothing and
/// exits 0 having run zero tests.
fn integration_target_name(file: &str) -> Option<String> {
    // A DECLARED `[[test]]` name wins outright: the manifest is the authority
    // on what Cargo builds, and the filename stem is not. `[[test]] name =
    // "acceptance" path = "tests/api_test.rs"` makes `--test api_test` a name
    // Cargo rejects, so this must be resolved BEFORE the rules below.
    if let Some(declared) = declared_integration_target(file) {
        return Some(declared);
    }
    let lowered = file.replace('\\', "/");
    let segments: Vec<&str> = lowered.split('/').collect();
    let tests_at = segments.iter().position(|segment| *segment == "tests")?;
    match &segments[tests_at + 1..] {
        // `tests/<name>.rs`
        [name] => Some(name.strip_suffix(".rs").unwrap_or(name).to_string()),
        // `tests/<dir>/…` — the DIRECTORY is the target name, and only when
        // its `main.rs` exists AND declares this file as a module.
        [dir, rest @ ..] => {
            let main_path = format!("{}/main.rs", segments[..tests_at + 2].join("/"));
            // `main_path` is built from `file`'s own segments, so it carries
            // `file`'s anchoring: the PDG stores ABSOLUTE paths (and
            // `call_sites` joins `root` for any relative one), which makes this
            // check CWD-independent. A purely relative path would be
            // CWD-anchored, but nothing could recover the crate root from it
            // anyway. Pinned by test, not papered over. (Kilo round 10)
            let main = std::fs::read_to_string(&main_path).ok()?;
            // The ENTRY POINT is always part of its own target: `main.rs` IS
            // the crate root, so it never needs a `mod main;` declaration.
            if rest.last().copied() == Some("main.rs") {
                return Some((*dir).to_string());
            }
            // Otherwise the file's module path relative to the directory:
            // `handshake.rs` is `mod handshake;`, `sub/mod.rs` is `mod sub;`.
            let stem = rest.last()?.strip_suffix(".rs").unwrap_or(rest.last()?);
            let stem = if stem == "mod" {
                // `sub/mod.rs` is declared as `mod sub;`.
                rest.get(rest.len().checked_sub(2)?).copied()?
            } else {
                stem
            };
            main.declares_module(stem).then(|| (*dir).to_string())
        }
        [] => None,
    }
}

/// Whether a crate root declares `mod <name>;` (or `pub mod <name>;`), i.e.
/// whether `name.rs` / `name/mod.rs` is compiled into THIS target.
///
/// Matched on the `mod` item's own identifier so a `mod handshake_tests;`
/// does not satisfy `handshake`, and a line inside a string or comment that
/// merely contains the word does not count. Attributes and visibility
/// prefixes before `mod` are tolerated.
///
/// This is a LINE-ORIENTED scan, not a Rust parser (Kilo round 11, id
/// 4698809204): it recognises `#[...]` / `#[path = "..."]` / `pub`,
/// `pub(crate)`, `pub(super)`, `pub(in path)` on one line, and an attribute
/// that spans several lines (`#[\n    path = "x"\n]`) only by skipping forward
/// to its closing `]`. Anything exotic — a `mod` inside a macro body, or a
/// `#[cfg_attr(...)]` that expands to `mod` — is beyond it. A false
/// negative here is safe (the caller falls back to `--tests`, which still
/// runs the test); a false positive would select a target without the test,
/// and that is the direction the exact-identifier match guards.
trait DeclaresModule {
    fn declares_module(&self, name: &str) -> bool;
}

impl DeclaresModule for str {
    fn declares_module(&self, name: &str) -> bool {
        self.lines().any(|line| {
            let line = line.trim_start();
            // Skip attributes (`#[path = "..."]`) and visibility (`pub`, `pub(crate)`).
            let mut rest = line;
            while let Some(after) = rest.strip_prefix('#') {
                rest = after.trim_start();
                // drop the `[...]` group
                match rest.find(']') {
                    Some(end) => rest = &rest[end + 1..],
                    None => return false,
                }
            }
            let rest = rest.trim_start();
            let rest = if let Some(after) = rest.strip_prefix("pub") {
                let after = after.trim_start();
                match after.strip_prefix('(') {
                    Some(inner) => match inner.find(')') {
                        Some(end) => &inner[end + 1..],
                        None => return false,
                    },
                    None => after,
                }
            } else {
                rest
            };
            let Some(after) = rest.trim_start().strip_prefix("mod") else {
                return false;
            };
            // `mod` must be a word boundary, not a prefix of `module`.
            if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                return false;
            }
            let after = after.trim_start();
            // The declared name, up to `;` or `{`.
            let end = after
                .find(|c: char| c == ';' || c == '{' || c.is_whitespace())
                .unwrap_or(after.len());
            after[..end] == *name
        })
    }
}

/// Test-only seam: `integration_target_name` is private and takes the raw
/// path the PDG stores, so the regression tests reach it directly.
#[cfg(test)]
pub(crate) fn integration_target_name_for_test(file: &str) -> Option<String> {
    integration_target_name(file)
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
    // `tests/` can appear at ANY depth below the crate root: `tests/api/main.rs`
    // and `tests/common/mod.rs` are integration-target files just as
    // `tests/api_test.rs` is (Codex round 8, id 4691102595). Only the
    // immediate-parent form was recognised, so a nested file fell through to
    // CargoUnit and `command_for` emitted `--lib`, which runs ZERO tests.
    let under_tests = segments
        .iter()
        .take(segments.len().saturating_sub(1))
        .any(|segment| *segment == "tests");
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

/// The nearest ancestor directory holding a `Cargo.toml`, starting at
/// `file`'s own directory. Bounded (16 levels) so a stray file above a
/// workspace cannot spin here; `None` means the file is not inside a crate.
fn cargo_package_dir(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?.to_path_buf();
    for _ in 0..16 {
        if dir.join("Cargo.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// A Cargo target that can own a unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoTarget {
    /// The package's library target (selected with `--lib`).
    Lib,
    /// A binary target, under the name Cargo gives it (`--bin <name>`).
    Bin(String),
}

/// Every target the package in `package_dir` defines, as
/// `(crate root file, target)`.
///
/// Cargo's target table is reproduced from the manifest text — no
/// `cargo metadata` process per lookup — because the selector has to be
/// exactly right: `--lib` for a test that lives in a binary runs ZERO tests
/// and exits 0, and `--bin <package>` is just as wrong when the binary
/// target carries its own name.
///
/// Reproduced rules (each verified against `cargo metadata`):
///
/// - `[lib]` with a `path` is the library, even when `src/lib.rs` also
///   exists (then `src/lib.rs` is not a target at all); `[lib]` without a
///   `path` is `src/lib.rs`. `autolib = false` disables the implicit one.
/// - a `[[bin]]` takes the `path` it declares; without one it lives at
///   `src/main.rs` when its name is the package name, else at
///   `src/bin/<name>.rs` or `src/bin/<name>/main.rs`. A `[[bin]]` without a
///   name is refused by Cargo and is skipped here.
/// - auto-discovery (`autobins`, on by default) adds `src/main.rs` under the
///   package name plus `src/bin/*.rs` and `src/bin/*/main.rs`, except for
///   paths an explicit `[[bin]]` already claimed (the explicit name wins).
///
/// A target whose file does not exist is dropped: Cargo refuses such a
/// manifest, and a root that cannot be read cannot own a test.
fn cargo_targets(package_dir: &Path) -> Vec<(PathBuf, CargoTarget)> {
    let Ok(text) = std::fs::read_to_string(package_dir.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let package = table.get("package");
    let package_name = package
        .and_then(|section| section.get("name"))
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let switch = |key: &str| -> bool {
        package
            .and_then(|section| section.get(key))
            .and_then(toml::Value::as_bool)
            .unwrap_or(true)
    };

    let mut targets: Vec<(PathBuf, CargoTarget)> = Vec::new();
    // A `[lib]` table declares the library explicitly; only its `path` is
    // optional (default `src/lib.rs`). Without the table, auto-discovery
    // decides, which `autolib = false` turns off.
    let lib_path = match table.get("lib") {
        Some(lib) => Some(
            lib.get("path")
                .and_then(toml::Value::as_str)
                .map(|path| package_dir.join(path))
                .unwrap_or_else(|| package_dir.join("src/lib.rs")),
        ),
        None => switch("autolib").then(|| package_dir.join("src/lib.rs")),
    };
    if let Some(path) = lib_path
        && path.is_file()
    {
        targets.push((path, CargoTarget::Lib));
    }

    // Explicit `[[bin]]` entries, and the paths they claim so auto-discovery
    // does not also register the same file under a second name.
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    for entry in table
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let path = match entry.get("path").and_then(toml::Value::as_str) {
            Some(explicit) => package_dir.join(explicit),
            None if name == package_name => package_dir.join("src/main.rs"),
            None => {
                let flat = package_dir.join("src/bin").join(format!("{name}.rs"));
                if flat.is_file() {
                    flat
                } else {
                    package_dir.join("src/bin").join(name).join("main.rs")
                }
            }
        };
        claimed.insert(path.clone());
        if path.is_file() {
            targets.push((path, CargoTarget::Bin(name.to_string())));
        }
    }

    if !switch("autobins") {
        return targets;
    }
    let main = package_dir.join("src/main.rs");
    if main.is_file() && !claimed.contains(&main) {
        targets.push((main, CargoTarget::Bin(package_name.clone())));
    }
    let Ok(entries) = std::fs::read_dir(package_dir.join("src/bin")) else {
        return targets;
    };
    let mut discovered: Vec<(PathBuf, CargoTarget)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let (root, name) = if path.extension().is_some_and(|ext| ext == "rs") && path.is_file() {
            // Build the name BEFORE moving `path` out: it must not borrow.
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            (path, name)
        } else if path.is_dir() && path.join("main.rs").is_file() {
            let name = path
                .file_name()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            (path.join("main.rs"), name)
        } else {
            continue;
        };
        discovered.push((root, CargoTarget::Bin(name)));
    }
    // Directory order is unspecified; a stable order keeps the command (and
    // the reported target) reproducible across machines.
    discovered.sort_by(|left, right| left.0.cmp(&right.0));
    for (root, target) in discovered {
        if !claimed.contains(&root) && !targets.iter().any(|(known, _)| known == &root) {
            targets.push((root, target));
        }
    }
    targets
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
    let mut modules = out_of_line_module_ancestry(&node.file_path)
        .map(|(_, path)| path)
        .unwrap_or_default();
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
/// Only the chain that actually reaches `file` is returned, together with
/// the crate ROOT file the chain starts from (which decides the Cargo
/// target: `main.rs` is a binary, `lib.rs` the library); an empty chain
/// means `file` IS the crate root. The walk is bounded (depth 32, 4,000
/// declarations) so a pathological tree cannot spin.
fn out_of_line_module_ancestry(file: &str) -> Option<(std::path::PathBuf, Vec<String>)> {
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
    // Every crate root the package has: the library, the default binary,
    // `src/bin/*.rs`, `src/bin/*/main.rs` and any `[[bin]]`/`[lib]` with an
    // explicit `path` — the same table Cargo builds (see [`cargo_targets`]).
    // A test in `main.rs` is reachable only from the `main.rs` root, so
    // stopping at the first existing root (as this once did) silently
    // classified binary tests as library tests.
    let roots: Vec<PathBuf> = cargo_targets(&package_dir)
        .into_iter()
        .map(|(root, _)| root)
        .collect();
    let same = |p: &Path| p.canonicalize().ok() == Some(canonical_target.clone());
    for root in &roots {
        if same(root) {
            return Some((root.clone(), Vec::new()));
        }
    }

    // Files still to expand, with the module path that declares them and
    // the crate root they descend from (which decides the Cargo target).
    let mut queue: Vec<(PathBuf, bool, Vec<String>, PathBuf)> = roots
        .iter()
        .map(|root| (root.clone(), true, Vec::new(), root.clone()))
        .collect();
    let mut visited = 0usize;
    while let Some((file, is_mod_rs, prefix, crate_root)) = queue.pop() {
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
                    return Some((crate_root.clone(), path));
                }
                let child_is_mod_rs =
                    resolved.file_name().and_then(|n| n.to_str()) == Some("mod.rs");
                queue.push((resolved, child_is_mod_rs, path, crate_root.clone()));
            }
        }
    }
    None
}

/// A `pytest` locator: the single node id that names one test, or the
/// ambiguity that has to be reported instead of a guessed id.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PytestLocator {
    /// Exactly one definition matched: run this node id.
    Id(String),
    /// Several same-named definitions matched and none could be selected
    /// positionally — every candidate is listed, none is picked.
    Ambiguous(Vec<String>),
}

/// The class ancestry of `fn_node`, outermost class first.
fn pytest_classes(
    fn_node: tree_sitter::Node<'_>,
    source: &crate::cli::mcp::call_sites::FileSource,
) -> Vec<String> {
    let mut classes: Vec<String> = Vec::new();
    let mut cursor = fn_node;
    while let Some(parent) = cursor.parent() {
        if parent.kind() == "class_definition"
            && let Some(class) = parent.child_by_field_name("name")
            && let Ok(class) = class.utf8_text(source.text.as_bytes())
        {
            classes.push(class.to_string());
        }
        cursor = parent;
    }
    classes.reverse();
    classes
}

/// Every `path::Class::test` node id that could be the definition named
/// `name`, deduplicated and in tree order.
fn pytest_candidates(
    file: &str,
    name: &str,
    source: &crate::cli::mcp::call_sites::FileSource,
) -> Vec<String> {
    let Some(tree) = source.tree.as_ref() else {
        return Vec::new();
    };
    let mut found: Vec<String> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition"
            && node
                .child_by_field_name("name")
                .and_then(|child| child.utf8_text(source.text.as_bytes()).ok())
                .is_some_and(|text| text == name)
        {
            let mut parts = vec![file.to_string()];
            parts.extend(pytest_classes(node, source));
            parts.push(name.to_string());
            let id = parts.join("::");
            if !found.contains(&id) {
                found.push(id);
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    found
}

/// A single valid pytest node id for a test: `path::test` for a module-level
/// function, `path::Class::test` for a method (nested classes joined by
/// `::`).
///
/// Same-named methods in different classes both reduce to `Class::test`, so
/// the definition is first selected POSITIONALLY from the PDG byte range
/// (clamped to the live tree, so a stale range degrades instead of
/// panicking). When position cannot decide — the range points at no
/// definition, or several classes share it — the result is
/// [`PytestLocator::Ambiguous`]: reporting the ambiguity is the only honest
/// answer, because silently running the last match can execute the wrong
/// test while the intended one goes untested.
fn pytest_locator(
    file: &str,
    name: &str,
    byte_range: (usize, usize),
    source: Option<&crate::cli::mcp::call_sites::FileSource>,
) -> PytestLocator {
    let Some(source) = source else {
        return PytestLocator::Id(format!("{file}::{name}"));
    };
    let Some(tree) = source.tree.as_ref() else {
        return PytestLocator::Id(format!("{file}::{name}"));
    };
    let mut candidates = pytest_candidates(file, name, source);
    if candidates.len() <= 1 {
        return match candidates.pop() {
            Some(id) => PytestLocator::Id(id),
            None => PytestLocator::Id(format!("{file}::{name}")),
        };
    }
    candidates.sort();
    // Several definitions share the name: prefer the one the index points
    // at. `node_for_range` floors to a char boundary, so a stale offset
    // inside a multi-byte character still resolves to an enclosing node; the
    // match may be the definition itself or its `name` identifier, so the
    // search walks up to the enclosing `function_definition`.
    let by_position = crate::cli::mcp::call_sites::node_for_range(tree.root_node(), byte_range)
        .and_then(|node| {
            let mut cursor = Some(node);
            while let Some(current) = cursor {
                if current.kind() == "function_definition"
                    && current
                        .child_by_field_name("name")
                        .and_then(|child| child.utf8_text(source.text.as_bytes()).ok())
                        .is_some_and(|text| text == name)
                {
                    return Some(current);
                }
                cursor = current.parent();
            }
            None
        })
        .map(|node| {
            let mut parts = vec![file.to_string()];
            parts.extend(pytest_classes(node, source));
            parts.push(name.to_string());
            parts.join("::")
        });
    match by_position {
        Some(id) if candidates.contains(&id) => PytestLocator::Id(id),
        _ => PytestLocator::Ambiguous(candidates),
    }
}

/// A single valid pytest node id, or the ambiguity text when the name alone
/// cannot name one test (see [`pytest_locator`]).
fn pytest_node_id(
    file: &str,
    name: &str,
    byte_range: (usize, usize),
    source: Option<&crate::cli::mcp::call_sites::FileSource>,
) -> String {
    match pytest_locator(file, name, byte_range, source) {
        PytestLocator::Id(id) => id,
        PytestLocator::Ambiguous(ids) => format!(
            "(ambiguous pytest id for {name} in {file}: {} — the index does not \
             say which definition this target is; look the test up by file and \
             line)",
            ids.join(" | ")
        ),
    }
}

/// The Cargo target that owns the unit test in `file`.
///
/// A unit test lives in exactly one target: the library, or a binary.
/// Selecting `--lib` for a test that lives in `src/main.rs` or in a module
/// owned by a `[[bin]]` runs ZERO tests and still exits 0, so a failing
/// binary test read as a pass. The owning target is the one whose crate root
/// the module chain starts at, looked up in the package's real target table
/// ([`cargo_targets`]) — not inferred from the root's file name, which
/// misreads `[lib] path = "core/api.rs"` as a binary whenever a `[[bin]]`
/// shares the package, and not from the package name, which is only the
/// binary's name when no `[[bin]]` renamed it.
fn cargo_unit_target(file: &str) -> Option<CargoTarget> {
    let (root, _) = out_of_line_module_ancestry(file)?;
    let package_dir = cargo_package_dir(Path::new(file))?;
    let canonical_root = root.canonicalize().ok()?;
    cargo_targets(&package_dir)
        .into_iter()
        .find(|(candidate, _)| candidate.canonicalize().ok() == Some(canonical_root.clone()))
        .map(|(_, target)| target)
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
            // Cargo names an integration target after the file stem
            // (`tests/api_test.rs` => `api_test`) — but a DIRECTORY target is
            // named after the directory: `tests/api/main.rs` is `--test api`,
            // and `--test main` runs nothing (Codex round 8, id 4691102595).
            match integration_target_name(file) {
                Some(target) => format!("cargo test {dash_p}--test {target} {}", node.name),
                // A file under `tests/<dir>/` with no `main.rs` beside it is a
                // shared HELPER module: it belongs to whichever targets `mod`
                // it, so `--test <dir>` would name a target Cargo does not
                // have. `--tests` runs every test target and the name filter
                // picks the test wherever it was compiled in — runnable, and
                // it does not claim a target that is not there. The filter is
                // a SUBSTRING, exactly like the target-selected branch above
                // (`-- --exact` would refuse the module-qualified path the
                // helper's test actually carries).
                None => format!("cargo test {dash_p}--tests {}", node.name),
            }
        }
        Some(TestRunner::CargoUnit) => {
            let path = source
                .and_then(|source| unit_module_path(source, node, &node.name))
                .unwrap_or_else(|| node.name.clone());
            match cargo_unit_target(file) {
                // The binary's OWN name: `--bin <package>` only works when
                // no `[[bin]]` renamed the target.
                Some(CargoTarget::Bin(bin)) => {
                    format!("cargo test {dash_p}--bin {bin} {path} -- --exact")
                }
                _ => format!("cargo test {dash_p}--lib {path} -- --exact"),
            }
        }
        Some(TestRunner::Pytest) => {
            // ONE executable node id. pytest's positional arguments are
            // `[file_or_dir]`, so a previous `file::test or file:12` form
            // made pytest treat the literal word `or` as a path: `ERROR:
            // file or directory not found: or`, nothing collected. Class
            // ancestry (`file::TestFoo::test_bar`) is required for a method
            // and comes from the live tree; a line number is not part of a
            // node id, so it is not appended.
            let locator = pytest_node_id(file, &node.name, node.byte_range, source);
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
    // `fn_start` is a PDG byte offset while `text` is the LIVE file; after an
    // edit the stale offset can sit inside a multi-byte character, and a
    // bare slice there panics mid-classification. Floor to a boundary.
    let before = &text[..crate::cli::mcp::call_sites::floor_char_boundary(text, fn_start)];
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

    fn parse_python(source: &str) -> crate::cli::mcp::call_sites::FileSource {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&crate::parse::traits::languages::python::language())
            .unwrap();
        crate::cli::mcp::call_sites::FileSource {
            text: source.to_string(),
            tree: parser.parse(source, None),
        }
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
        command_for_test_at(dir, &format!("src/{rel}"), name)
    }

    fn command_for_test_at(dir: &tempfile::TempDir, rel: &str, name: &str) -> String {
        let file = dir.path().join(rel);
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

    /// A pytest method needs its class in the node id, read from the live
    /// tree; a module-level test stays `file::test`. The command must be a
    /// single node id with no `or` and no line suffix.
    #[test]
    fn test_pytest_node_id_includes_class_ancestry_and_is_executable() {
        let dir = manifest_dir("pyproj");
        let file = dir.path().join("tests").join("core_test.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let source = "class TestOuter:\n    class TestInner:\n        def test_nested(self):\n            assert True\n\n    def test_method(self):\n        assert True\n\ndef test_plain():\n    assert True\n";
        std::fs::write(&file, source).unwrap();
        let src = parse_python(source);
        let py = |name: &str| {
            node(
                name,
                &file.display().to_string(),
                "python",
                (source.find(name).unwrap(), source.len()),
            )
        };
        let shown = file.display().to_string();
        assert_eq!(
            command_for(&py("test_nested"), Some(3), Some(&src)),
            format!("pytest {shown}::TestOuter::TestInner::test_nested")
        );
        assert_eq!(
            command_for(&py("test_method"), Some(6), Some(&src)),
            format!("pytest {shown}::TestOuter::test_method")
        );
        assert_eq!(
            command_for(&py("test_plain"), Some(9), Some(&src)),
            format!("pytest {shown}::test_plain")
        );
        // Without a tree it degrades to the function alone — still one id.
        assert_eq!(
            command_for(&py("test_method"), Some(6), None),
            format!("pytest {shown}::test_method")
        );
        // Execute the generated command with real pytest (if installed) on a
        // deliberately failing method: it must collect and run that test.
        if std::process::Command::new("python3")
            .arg("-m")
            .arg("pytest")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let failing = source.replace(
                "    def test_method(self):\n        assert True",
                "    def test_method(self):\n        assert False",
            );
            std::fs::write(&file, failing).unwrap();
            let out = std::process::Command::new("python3")
                .args([
                    "-m",
                    "pytest",
                    "-q",
                    "tests/core_test.py::TestOuter::test_method",
                ])
                .current_dir(dir.path())
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                text.contains("1 failed"),
                "pytest must run exactly that test: {text}"
            );
            assert!(!out.status.success());
        }
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

    /// A unit test in `src/main.rs` belongs to the BINARY target. `--lib`
    /// runs zero tests there and exits 0, so a failing binary test read as a
    /// pass; the command must select `--bin <package>` instead.
    #[test]
    fn test_binary_unit_tests_select_the_bin_target() {
        let dir = manifest_dir("binpkg");
        let src = dir.path().join("src");
        std::fs::write(
            src.join("main.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_bin() {\n        assert!(true);\n    }\n}\n\nfn main() {}\n",
        )
        .unwrap();
        assert_eq!(
            command_for_test_in(&dir, "main.rs", "test_in_bin"),
            "cargo test -p binpkg --bin binpkg tests::test_in_bin -- --exact"
        );
        // A `[[bin]]` with an explicit path is a binary too.
        std::fs::write(src.join("lib.rs"), "pub mod tool;\n").unwrap();
        std::fs::write(
            src.join("tool.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_tool() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub mod tool;\n#[test]\nfn test_in_lib() {\n    assert!(true);\n}\n",
        )
        .unwrap();
        assert_eq!(
            command_for_test_in(&dir, "lib.rs", "test_in_lib"),
            "cargo test -p binpkg --lib test_in_lib -- --exact"
        );
        assert_eq!(
            command_for_test_in(&dir, "tool.rs", "test_in_tool"),
            "cargo test -p binpkg --lib tool::tests::test_in_tool -- --exact"
        );

        // Real cargo, with BOTH a lib and a bin in the package (the case the
        // reviewer describes): make the binary test fail. The generated
        // command must run it and fail...
        std::fs::write(
            src.join("main.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_bin() {\n        assert!(false);\n    }\n}\n\nfn main() {}\n",
        )
        .unwrap();
        let command = command_for_test_in(&dir, "main.rs", "test_in_bin");
        // Run the generated command directly (the generated form has no
        // quoting), so the test does not depend on `sh` finding cargo.
        let cargo = std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|d| d.join("cargo"))
                    .find(|c| c.is_file())
            })
            .unwrap_or_default();
        let Some(cargo) = cargo else {
            eprintln!("skipping real-cargo check: cargo not on PATH");
            return;
        };
        let run = |cmd: &str| {
            // `cargo` is the first word; resolve it to the absolute path found
            // above so the test does not depend on the harness's PATH lookup.
            let words = cmd.split_whitespace().skip(1);
            let program = cargo.clone();
            let out = std::process::Command::new(program)
                .args(words)
                .current_dir(dir.path())
                .env("CARGO_TERM_COLOR", "never")
                .output()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), text)
        };
        let (ok, text) = run(&command);
        assert!(
            text.contains("test tests::test_in_bin ... FAILED"),
            "binary test must run ({command}): {text}"
        );
        assert!(!ok, "a failing binary test must fail the run");
        // ...while the old `--lib` form runs ZERO tests and exits 0, which is
        // exactly the silent pass the finding describes.
        let (ok, text) = run("cargo test -p binpkg --lib tests::test_in_bin -- --exact");
        assert!(
            ok && text.contains("0 passed"),
            "control: --lib runs no binary tests: {text}"
        );
    }

    /// A stale PDG offset must not panic the tests classifier (same class as
    /// the caller-list fix): the indexed file's `fn` byte offset, applied to
    /// a live file whose leading comment grew by a multi-byte character,
    /// lands inside that character.
    #[test]
    fn test_tests_classifier_tolerates_a_stale_multibyte_offset() {
        // Indexed: `//` + 9x `x` + `\n` (12 bytes) => `fn` starts at byte 12.
        let indexed = "//xxxxxxxxx\nfn test_x() {}\n";
        let stale = indexed.find("fn test_x").unwrap();
        assert_eq!(stale, 12);
        // Live: the last two `x` became `é` (2 bytes each), so `é` spans
        // bytes 9..13 and the stale byte 12 is INSIDE the second `é`.
        let live = "//xxxxxxx\u{e9}\u{e9}\nfn test_x() {}\n";
        assert!(
            !live.is_char_boundary(stale),
            "fixture must put the stale offset inside a multi-byte char"
        );
        // Must not panic: the offset floors to byte 9 and the scan simply
        // finds no attribute there ("no evidence"), never a crash.
        let _ = attributes_say_test(live, stale);
        // With an attribute above the fn and the REAL offset, it classifies.
        let with_attr = format!("#[test]\n{live}");
        let real = with_attr.find("fn test_x").unwrap();
        assert!(attributes_say_test(&with_attr, real).is_some());
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

    /// A manifest can RENAME an integration target:
    /// `[[test]] name = "acceptance" path = "tests/api_test.rs"`.
    ///
    /// The filename stem is then NOT a target — `cargo test --test api_test`
    /// is rejected with `no test target named api_test`, listing `acceptance`.
    /// Verified against real cargo in both directions; the declared name is
    /// the only answer that runs the tests.
    #[test]
    fn test_manifest_declared_test_target_wins_over_the_filename_stem() {
        let dir = manifest_dir("proj");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"acceptance\"\npath = \"tests/api_test.rs\"\n",
        )
        .unwrap();
        let tests = dir.path().join("tests");
        std::fs::create_dir_all(&tests).unwrap();
        let file = tests.join("api_test.rs");
        std::fs::write(&file, "#[test]\nfn test_api() {}\n").unwrap();

        assert_eq!(
            integration_target_name(&file.display().to_string()).as_deref(),
            Some("acceptance"),
            "the declared [[test]] name is the target Cargo builds"
        );

        let test_fn = node("test_api", &file.display().to_string(), "rust", (0, 1));
        let cmd = command_for(&test_fn, None, None);
        assert_eq!(
            cmd, "cargo test -p proj --test acceptance test_api",
            "never the filename stem: {cmd}"
        );
    }

    /// A `[[test]]` without a `path` defaults to `tests/<name>.rs`, so that
    /// file belongs to the declared target too.
    #[test]
    fn test_manifest_declared_target_without_a_path_claims_tests_named_after_it() {
        let dir = manifest_dir("proj");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"smoke\"\n",
        )
        .unwrap();
        let tests = dir.path().join("tests");
        std::fs::create_dir_all(&tests).unwrap();
        let file = tests.join("smoke.rs");
        std::fs::write(&file, "#[test]\nfn test_smoke() {}\n").unwrap();

        assert_eq!(
            integration_target_name(&file.display().to_string()).as_deref(),
            Some("smoke"),
            "the default path tests/<name>.rs still resolves to the target"
        );
    }

    /// The declaration covers the file it names, and nothing else: a sibling
    /// `tests/other_test.rs` keeps filename inference (its own target).
    #[test]
    fn test_manifest_declaration_does_not_capture_undeclared_siblings() {
        let dir = manifest_dir("proj");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"acceptance\"\npath = \"tests/api_test.rs\"\n",
        )
        .unwrap();
        let tests = dir.path().join("tests");
        std::fs::create_dir_all(&tests).unwrap();
        std::fs::write(tests.join("api_test.rs"), "").unwrap();
        let other = tests.join("other_test.rs");
        std::fs::write(&other, "").unwrap();

        assert_eq!(
            integration_target_name(&other.display().to_string()).as_deref(),
            Some("other_test"),
            "an undeclared sibling is still its own auto-discovered target"
        );
    }

    /// Codex round 8 (id 4691102595): `tests/<dir>/` with a `main.rs` is ONE
    /// Cargo target named after the DIRECTORY. The selector used the file stem,
    /// so `tests/api/main.rs` produced `--test main`, which matches no target
    /// and exits 0 having run nothing — a non-existent test read as a pass.
    #[test]
    fn test_directory_integration_target_is_named_after_the_directory() {
        let dir = manifest_dir("proj");
        let api = dir.path().join("tests").join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("main.rs"), "mod handshake;\n").unwrap();
        std::fs::write(api.join("handshake.rs"), "fn helper() {}\n").unwrap();

        // The entry point and a sibling module both belong to target `api`.
        for file in ["main.rs", "handshake.rs"] {
            let test_fn = node(
                "test_handshake",
                &api.join(file).display().to_string(),
                "rust",
                (0, 1),
            );
            let cmd = command_for(&test_fn, None, None);
            assert_eq!(
                cmd, "cargo test -p proj --test api test_handshake",
                "{file} belongs to the `api` target: {cmd}"
            );
        }
    }

    /// The other half of the same review finding: without a `main.rs`,
    /// `tests/<dir>/` is NOT a target — its files are helper modules compiled
    /// into whichever targets `mod` them. Naming `--test <dir>` there would
    /// point at a target Cargo does not have, so the command falls back to
    /// `--tests`, which runs the test wherever it was compiled in.
    #[test]
    fn test_helper_module_without_main_rs_falls_back_to_all_test_targets() {
        let dir = manifest_dir("proj");
        let common = dir.path().join("tests").join("common");
        std::fs::create_dir_all(&common).unwrap();
        std::fs::write(common.join("mod.rs"), "fn fixture() {}\n").unwrap();

        let test_fn = node(
            "test_fixture",
            &common.join("mod.rs").display().to_string(),
            "rust",
            (0, 1),
        );
        let cmd = command_for(&test_fn, None, None);
        assert_eq!(
            cmd, "cargo test -p proj --tests test_fixture",
            "a helper module has no target of its own: {cmd}"
        );
    }

    /// CodeRabbit round 10 (id 4697090278): the other half. `tests/api/main.rs`
    /// EXISTS, so the directory IS a target — but it does not declare THIS
    /// file. `tests/api/shared_test.rs` is imported by `tests/other_test.rs`
    /// via `#[path]`, so it is a helper that belongs to `other_test`, not to
    /// `api`. Claiming `--test api` selected a target that does not contain
    /// the test, and the command exited 0 having run nothing.
    #[test]
    fn test_a_shared_file_beside_a_main_rs_that_does_not_declare_it_falls_back() {
        let dir = manifest_dir("proj");
        let tests = dir.path().join("tests");
        let api = tests.join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(api.join("shared_test.rs"), "#[test]\nfn test_shared() {}\n").unwrap();
        std::fs::write(
            tests.join("other_test.rs"),
            "#[path = \"api/shared_test.rs\"]\nmod shared_test;\n",
        )
        .unwrap();

        let test_fn = node(
            "test_shared",
            &api.join("shared_test.rs").display().to_string(),
            "rust",
            (0, 1),
        );
        assert_eq!(
            integration_target_name_for_test(&api.join("shared_test.rs").display().to_string()),
            None,
            "a main.rs that does not declare the file does not own it"
        );
        let cmd = command_for(&test_fn, None, None);
        assert_eq!(
            cmd, "cargo test -p proj --tests test_shared",
            "the fallback runs the test wherever it was compiled in: {cmd}"
        );
    }

    /// Kilo round 10 (id 4696869415) flagged that `main.rs` is checked with a
    /// bare `is_file()`, which is CWD-relative for a relative path. Verified
    /// rather than assumed:
    ///
    /// * for an ABSOLUTE `file` — what the PDG stores, and what `call_sites`
    ///   produces by joining `root` — the manifest is built from the same
    ///   segments and is absolute too, so the check is already CWD-independent
    ///   and the finding is latent, not live. No change needed, and this test
    ///   pins that from two different CWDs.
    /// * for a purely RELATIVE `file` the check IS CWD-anchored — but no code
    ///   could fix that, because nothing can recover the crate root from
    ///   `tests/api/handshake.rs` alone. Attempting a root-join made the
    ///   relative case no better while adding nothing for the absolute one, so
    ///   it was reverted rather than shipped as dead code.
    ///
    /// `set_current_dir` is process-global: serialised on a lock and restored
    /// on drop, including on panic.
    #[test]
    fn test_a_directory_integration_target_is_found_from_any_cwd() {
        // `set_current_dir` is process-global, and the lock is the CRATE-WIDE
        // one (CodeRabbit round 11, id 4698955238): a module-local `CWD_LOCK`
        // only excluded this module's tests, so `registry_test`'s CWD read
        // could still observe this change and compare against the wrong
        // directory. Same lock as every other process-global mutation.
        let _guard = crate::test_env::EnvLock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        struct Restore(std::path::PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let original = std::env::current_dir().unwrap();
        let _restore = Restore(original);

        let dir = manifest_dir("proj");
        let api = dir.path().join("tests").join("api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("main.rs"), "mod handshake;\n").unwrap();
        std::fs::write(api.join("handshake.rs"), "fn helper() {}\n").unwrap();

        // The contract that matters: an ABSOLUTE path is CWD-independent and
        // the directory target is recognised from anywhere.
        let absolute = api.join("handshake.rs").display().to_string();
        let elsewhere = tempfile::tempdir().unwrap();
        let from_root = integration_target_name(&absolute);
        std::env::set_current_dir(elsewhere.path()).unwrap();
        let from_elsewhere = integration_target_name(&absolute);
        assert_eq!(
            from_root.as_deref(),
            Some("api"),
            "recognised from the project: {from_root:?}"
        );
        assert_eq!(
            from_elsewhere, from_root,
            "an absolute path must not depend on the process CWD: root={from_root:?} elsewhere={from_elsewhere:?}"
        );

        // The stated limitation: a bare relative path is CWD-anchored, and no
        // crate-root resolution can rescue it. It resolves only while the CWD
        // is the project.
        let relative = "tests/api/handshake.rs";
        assert!(
            integration_target_name(relative).is_none(),
            "a bare relative path has no crate root to resolve against"
        );
        std::env::set_current_dir(dir.path()).unwrap();
        assert_eq!(
            integration_target_name(relative).as_deref(),
            Some("api"),
            "the same relative path resolves once the CWD is the project"
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
        // A single executable node id: never `a or b` (pytest would take the
        // literal `or` as a path) and no `:line` suffix (not a node id).
        assert_eq!(
            command_for(&py, Some(12), None),
            "pytest /proj/tests/core_test.py::test_round_trip"
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

    /// Same-named methods in different classes must not be conflated. The
    /// intended definition is selected POSITIONALLY from the PDG byte range;
    /// when position cannot decide, the ambiguity is reported instead of a
    /// guessed id (the traversal used to return whichever match it saw last,
    /// so TestA's test could be reported as TestB's — or vice versa).
    #[test]
    fn test_pytest_same_named_methods_in_different_classes_are_disambiguated() {
        let dir = manifest_dir("pyshared");
        let file = dir.path().join("tests").join("shared_test.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let source = "class TestFirst:\n    def test_shared(self):\n        assert True\n\nclass TestSecond:\n    def test_shared(self):\n        assert True\n";
        std::fs::write(&file, source).unwrap();
        let src = parse_python(source);
        let shown = file.display().to_string();
        let first = source.find("def test_shared").unwrap();
        let second = source[first + 1..].find("def test_shared").unwrap() + first + 1;
        // A point INSIDE each definition. The PDG range is the definition's
        // span up to its end byte, but the selection must only depend on
        // containment, so a degenerate point inside the body is enough.
        let at = |start: usize| node("test_shared", &shown, "python", (start, start));
        assert_eq!(
            command_for(&at(first + "def ".len()), Some(2), Some(&src)),
            format!("pytest {shown}::TestFirst::test_shared")
        );
        assert_eq!(
            command_for(&at(second + "def ".len()), Some(6), Some(&src)),
            format!("pytest {shown}::TestSecond::test_shared")
        );
        // Offset 0 lands on the class header, not a definition: the id must
        // NOT be guessed — both candidates are disclosed instead.
        let ambiguous = command_for(&at(0), Some(1), Some(&src));
        assert!(
            ambiguous.starts_with("pytest (ambiguous pytest id for test_shared"),
            "ambiguous: {ambiguous}"
        );
        assert!(ambiguous.contains(&format!("{shown}::TestFirst::test_shared")));
        assert!(ambiguous.contains(&format!("{shown}::TestSecond::test_shared")));

        // Execute the generated command with real pytest (if installed) on a
        // failing FIRST definition: the intended test must be the one that
        // runs, and the run must fail.
        if std::process::Command::new("python3")
            .arg("-m")
            .arg("pytest")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            let failing = source.replacen(
                "    def test_shared(self):\n        assert True",
                "    def test_shared(self):\n        assert False",
                1,
            );
            std::fs::write(&file, &failing).unwrap();
            let failing_src = parse_python(&failing);
            let command = command_for(&at(first + "def ".len()), Some(2), Some(&failing_src));
            assert_eq!(command, format!("pytest {shown}::TestFirst::test_shared"));
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(dir.path())
                .output()
                .expect("pytest runnable");
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                text.contains("1 failed"),
                "the intended definition must run: {text}"
            );
            assert!(!out.status.success());
        }
    }

    /// CR-R4-2: a `[lib]` that sits next to a `[[bin]]` is still the library.
    /// The library used to be recognised only while no `[[bin]]` existed, so
    /// every package with both classified its library tests as binary tests
    /// (and then ran zero tests under `--bin`).
    ///
    /// CR-R4-3: `--bin` must carry the OWNING binary target's name. Using the
    /// package name is right only when no `[[bin]]` renamed the target.
    #[test]
    fn test_lib_target_survives_a_coexisting_bin_target() {
        let dir = manifest_dir("both");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"both\"\nversion = \"0.1.0\"\n\n\
             [lib]\npath = \"core/api.rs\"\n\n\
             [[bin]]\nname = \"runner\"\npath = \"src/entry.rs\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(
            dir.path().join("core/api.rs"),
            "#[test]\nfn test_in_lib() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/entry.rs"),
            "#[test]\nfn test_in_bin() {}\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        std::fs::write(
            dir.path().join("src/bin/worker.rs"),
            "#[test]\nfn test_in_worker() {}\n",
        )
        .unwrap();

        // The library stays the library despite the coexisting `[[bin]]`.
        assert_eq!(
            command_for_test_at(&dir, "core/api.rs", "test_in_lib"),
            "cargo test -p both --lib test_in_lib -- --exact"
        );
        // A `[[bin]]` with its own name and path is selected by THAT name.
        assert_eq!(
            command_for_test_at(&dir, "src/entry.rs", "test_in_bin"),
            "cargo test -p both --bin runner test_in_bin -- --exact"
        );
        // An auto-discovered `src/bin/<name>.rs` binary carries its stem.
        assert_eq!(
            command_for_test_at(&dir, "src/bin/worker.rs", "test_in_worker"),
            "cargo test -p both --bin worker test_in_worker -- --exact"
        );
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
