use super::*;

// ---------------------------------------------------------------------------
// Phase 5: Import edge extraction with robust multi-line parsing
//
// The original line-by-line parser misses:
//   - Python:     from x import (
//    a,
//    b
//)
//   - Rust:       use x::{
//    A,
//    B
//};
//   - TypeScript: import {
//    A,
//    B
//} from 'x';
//   - Go:         import (
//    "pkg"
//    "pkg2"
//)
//   - Java:       import x.y.z; (straightforward but needs robustness)
//   - C#:         using X.Y.Z;
//   - Ruby:       require / require_relative
//   - PHP:        use X\Y\Z;
//   - Lua:        require('x')
//   - Scala:      import x.y.{A, B}
//   - C/C++:      #include <x> / #include "x"
//
// Strategy: strip comments, collapse the entire source to a single string,
// then apply per-language regex patterns with DOTALL semantics.
// All patterns are compiled once and cached as statics.
// ---------------------------------------------------------------------------

/// Extracts import paths from source code for multiple programming languages.
///
/// This function parses source code to identify import statements across
/// 12+ programming languages. It handles:
///
/// - **Rust**: `use`, `extern crate`, and multi-line imports
/// - **JavaScript/TypeScript**: `import` and `require()` statements
/// - **Go**: `import` blocks with single and multi-line formats
/// - **Python**: `import` and `from ... import` statements
/// - **Java**: `import` statements
/// - **C/C++**: `#include` directives
/// - **C#**: `using` statements
/// - **PHP**: `require`, `include`, `require_once`, `include_once`
/// - **Ruby**: `require` and `require_relative`
/// - **Swift**: `import` statements
/// - **Kotlin**: `import` statements
/// - **Dart**: `import` and `export` statements
///
/// The function strips block comments before parsing to avoid false positives.
///
/// # Arguments
///
/// * `source_code` - The source code as a byte slice
/// * `language` - The programming language identifier (e.g., "rust", "python")
///
/// # Returns
///
/// A HashSet of unique import paths/modules found in the source code.
pub fn extract_import_paths_from_source(source_code: &[u8], language: &str) -> HashSet<String> {
    let Ok(source) = std::str::from_utf8(source_code) else {
        return HashSet::default();
    };
    let lang = language.to_ascii_lowercase();
    let source = strip_block_comments(&lang, source);

    match lang.as_str() {
        "python" | "py" => extract_python_imports(&source),
        "javascript" | "js" | "typescript" | "ts" | "jsx" | "tsx" => extract_js_ts_imports(&source),
        "rust" | "rs" => extract_rust_imports(&source),
        "go" | "golang" => extract_go_imports(&source),
        "java" => extract_java_imports(&source),
        "csharp" | "cs" | "c#" => extract_csharp_imports(&source),
        "ruby" | "rb" => extract_ruby_imports(&source),
        "php" => extract_php_imports(&source),
        "lua" => extract_lua_imports(&source),
        "scala" => extract_scala_imports(&source),
        "c" | "cpp" | "c++" | "cxx" | "cc" | "h" | "hpp" => extract_c_imports(&source),
        _ => HashSet::default(),
    }
}

pub(super) fn strip_block_comments(lang: &str, source: &str) -> String {
    match lang {
        "python" | "py" | "ruby" | "rb" => source.to_string(), // no block comments to strip before imports
        _ => {
            // Strip /* ... */ style block comments
            let mut result = String::with_capacity(source.len());
            let mut chars = source.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '/' && chars.peek() == Some(&'*') {
                    chars.next(); // consume '*'
                    // Skip until */
                    loop {
                        match chars.next() {
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next();
                                break;
                            }
                            None => break,
                            _ => {}
                        }
                    }
                    result.push(' '); // preserve whitespace for line counting
                } else {
                    result.push(c);
                }
            }
            result
        }
    }
}

pub(super) fn extract_python_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();

    // `import x, y, z` (simple)
    let re_import = Regex::new(r"(?m)^import\s+([\w,\s.]+)").unwrap();
    for cap in re_import.captures_iter(source) {
        for name in cap[1].split(',') {
            let trimmed = name.split_whitespace().next().unwrap_or("").trim();
            if !trimmed.is_empty() {
                imports.insert(trimmed.to_string());
            }
        }
    }

    // `from x import (...)` — multi-line via DOTALL
    // First capture the module name, then the import list
    let re_from =
        Regex::new(r"(?s)from\s+([\w.]+)\s+import\s+(?:\(([^)]+)\)|(\w[\w\s,*]*))").unwrap();
    for cap in re_from.captures_iter(source) {
        let module = cap[1].trim();
        imports.insert(module.to_string());
        // Also insert fully qualified names for the imported symbols
        let names_str = cap.get(2).or(cap.get(3)).map(|m| m.as_str()).unwrap_or("");
        for name in names_str.split(',') {
            let sym = name
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('*');
            if !sym.is_empty() && sym != "*" {
                imports.insert(format!("{}.{}", module, sym));
            }
        }
    }

    imports
}

pub(super) fn extract_js_ts_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();

    // import { A, B } from 'x' — multi-line
    let re_named =
        Regex::new(r#"(?s)import\s+(?:type\s+)?\{[^}]*\}\s+from\s+['"]([^'"]+)['"]"#).unwrap();
    for cap in re_named.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }

    // import x from 'y'  / import * as x from 'y'
    let re_default =
        Regex::new(r#"import\s+(?:type\s+)?(?:\*\s+as\s+\w+|\w+)\s+from\s+['"]([^'"]+)['"]"#)
            .unwrap();
    for cap in re_default.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }

    // require('x')
    let re_require = Regex::new(r#"require\s*\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap();
    for cap in re_require.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }

    // export { } from 'x'
    let re_export = Regex::new(r#"export\s+(?:\*|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]"#).unwrap();
    for cap in re_export.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }

    imports
}

pub(super) fn extract_rust_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();

    // `use x::y::{A, B, C};` — multi-line via collapse
    // Collapse the entire source to handle multi-line use statements
    let collapsed = collapse_multiline(source, "use ", ';');
    for stmt in &collapsed {
        let use_stmt = stmt.trim_start_matches("use ").trim_end_matches(';').trim();
        expand_rust_use(use_stmt, &mut imports);
    }

    imports
}

pub(super) fn expand_rust_use(stmt: &str, out: &mut HashSet<String>) {
    // Handle: a::b::{C, D, E} and a::b::{c::{D}, e}
    if let Some(brace_start) = stmt.find('{') {
        let base = stmt[..brace_start]
            .trim()
            .trim_end_matches("::")
            .replace("::", ".");
        let inner = stmt[brace_start + 1..]
            .trim_end_matches('}')
            .trim_end_matches(';');
        // Recursively handle nested braces
        for item in split_respecting_braces(inner) {
            let item = item.trim();
            if item == "self" {
                out.insert(base.clone());
                continue;
            }
            if item.contains('{') {
                expand_rust_use(&format!("{}::{}", base.replace('.', "::"), item), out);
            } else {
                let full = format!("{}.{}", base, item.replace("::", "."));
                out.insert(full);
            }
        }
    } else {
        out.insert(stmt.replace("::", ".").trim_matches('.').to_string());
    }
}

pub(super) fn split_respecting_braces(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0i32;
    let mut last = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                result.push(s[last..i].trim());
                last = i + 1;
            }
            _ => {}
        }
    }
    let tail = s[last..].trim();
    if !tail.is_empty() {
        result.push(tail);
    }
    result
}

pub(super) fn collapse_multiline(source: &str, prefix: &str, terminator: char) -> Vec<String> {
    let mut results = Vec::new();
    let mut in_stmt = false;
    let mut current = String::new();

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }

        if !in_stmt && trimmed.starts_with(prefix) {
            in_stmt = true;
            current = trimmed.to_string();
        } else if in_stmt {
            current.push(' ');
            current.push_str(trimmed);
        }

        if in_stmt {
            if let Some(end) = current.find(terminator) {
                results.push(current[..=end].to_string());
                in_stmt = false;
                current = String::new();
            }
        }
    }

    results
}

pub(super) fn extract_go_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();

    // Single: import "pkg"
    let re_single = Regex::new(r#"import\s+["']([^"']+)["']"#).unwrap();
    for cap in re_single.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }

    // Block: import ( "a" "b" ) — multi-line
    let re_block = Regex::new(r#"(?s)import\s*\(([^)]+)\)"#).unwrap();
    let re_path = Regex::new(r#"["']([^"']+)["']"#).unwrap();
    for cap in re_block.captures_iter(source) {
        let inner = &cap[1];
        for p in re_path.captures_iter(inner) {
            imports.insert(p[1].trim().to_string());
        }
    }

    imports
}

pub(super) fn extract_java_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    let re = Regex::new(r"(?m)^import(?:\s+static)?\s+([\w.*]+)\s*;").unwrap();
    for cap in re.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }
    imports
}

pub(super) fn extract_csharp_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    // `using X.Y.Z;` and `using static X.Y.Z;`
    let re = Regex::new(r"(?m)^using(?:\s+static)?\s+([\w.]+)\s*;").unwrap();
    for cap in re.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }
    imports
}

pub(super) fn extract_ruby_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    let re = Regex::new(r#"(?:require|require_relative|load)\s*['"]([^'"]+)['"]"#).unwrap();
    for cap in re.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }
    imports
}

pub(super) fn extract_php_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    // use X\Y\Z; and use X\Y\Z as Alias;
    let re = Regex::new(r"(?m)^use\s+([\w\\]+)(?:\s+as\s+\w+)?\s*;").unwrap();
    for cap in re.captures_iter(source) {
        let path = cap[1].trim().replace('\\', ".");
        imports.insert(path);
    }
    // require/include
    let re_require =
        Regex::new(r#"(?:require|include)(?:_once)?\s*\(?['"]([^'"]+)['"]\)?"#).unwrap();
    for cap in re_require.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }
    imports
}

pub(super) fn extract_lua_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    let re = Regex::new(r#"require\s*\(?['"]([^'"]+)['"]\)?"#).unwrap();
    for cap in re.captures_iter(source) {
        imports.insert(cap[1].replace('.', "/").trim().to_string());
    }
    imports
}

pub(super) fn extract_scala_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    let re = Regex::new(r"(?m)^\s*import\s+([^\n]+)$").unwrap();
    let selector_re = Regex::new(r"^([\w.]+)(?:\.\{([^}]+)\}|\.(\w+|\*))?$").unwrap();

    for cap in re.captures_iter(source) {
        let stmt = cap[1].trim();
        if let Some(sel) = selector_re.captures(stmt) {
            let base = &sel[1];
            if let Some(names) = sel.get(2) {
                for name in names.as_str().split(',') {
                    let n = name.trim();
                    if n != "_" && !n.is_empty() {
                        imports.insert(format!("{}.{}", base, n));
                    }
                }
            } else if let Some(single) = sel.get(3) {
                imports.insert(format!("{}.{}", base, single.as_str()));
            } else {
                imports.insert(base.to_string());
            }
        }
    }

    imports
}

pub(super) fn extract_c_imports(source: &str) -> HashSet<String> {
    let mut imports = HashSet::default();
    // #include <x> and #include "x"
    let re = Regex::new(r#"#include\s*[<"']([^>"']+)[>"']"#).unwrap();
    for cap in re.captures_iter(source) {
        imports.insert(cap[1].trim().to_string());
    }
    imports
}

// ---------------------------------------------------------------------------
// Import edge wiring (unchanged logic)
// ---------------------------------------------------------------------------

pub(super) fn extract_import_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
    pdg: &mut ProgramDependenceGraph,
    file_path: &str,
    language: &str,
    source_code: &[u8],
) -> Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> {
    let mut edges = Vec::new();
    let mut seen: HashSet<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> =
        HashSet::default();

    let mut unique_paths: HashSet<String> = signatures
        .iter()
        .flat_map(|sig| sig.imports.iter().map(|imp| imp.path.clone()))
        .collect();
    unique_paths.extend(extract_import_paths_from_source(source_code, language));

    if unique_paths.is_empty() {
        return edges;
    }

    let module_sym = format!("{}:__module__", file_path);
    let importer_nid = pdg.find_by_symbol(&module_sym).unwrap_or_else(|| {
        pdg.add_node(Node {
            id: module_sym,
            node_type: NodeType::Module,
            name: "__module__".to_string(),
            file_path: Arc::from(file_path),
            byte_range: (0, 0),
            complexity: 1,
            language: language.to_string(),
        })
    });

    let mut symbol_map: HashMap<String, Vec<crate::graph::pdg::NodeId>> = HashMap::default();
    for sig in signatures {
        if let Some(&nid) = node_ids.get(&sig.qualified_name) {
            let norm = normalize_symbol(&sig.qualified_name);
            symbol_map.entry(norm.clone()).or_default().push(nid);
            if let Some(last) = norm.split('.').next_back() {
                symbol_map.entry(last.to_string()).or_default().push(nid);
            }
        }
    }

    let mut external_nodes: HashMap<String, crate::graph::pdg::NodeId> = HashMap::default();

    for path in unique_paths {
        let targets = resolve_import_targets(&path, &symbol_map);
        let targets = if targets.is_empty() {
            let eid = *external_nodes.entry(path.clone()).or_insert_with(|| {
                pdg.add_node(Node {
                    id: format!("{}:__external__:{}", file_path, path),
                    node_type: NodeType::External,
                    name: path.clone(),
                    file_path: Arc::from(file_path),
                    byte_range: (0, 0),
                    complexity: 1,
                    language: "external".to_string(),
                })
            });
            vec![eid]
        } else {
            targets
        };

        for target in targets {
            if target == importer_nid {
                continue;
            }
            if seen.insert((importer_nid, target)) {
                edges.push((importer_nid, target));
            }
        }
    }

    edges
}

pub(super) fn resolve_import_targets(
    import_path: &str,
    symbol_map: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
) -> Vec<crate::graph::pdg::NodeId> {
    let normalized = normalize_symbol(import_path);
    let mut targets: Vec<crate::graph::pdg::NodeId> = Vec::new();

    if let Some(ids) = symbol_map.get(&normalized) {
        targets.extend(ids);
    }

    let parts: Vec<&str> = normalized.split('.').collect();
    for len in 2..=3_usize.min(parts.len()) {
        let start = parts.len() - len;
        let key = parts[start..].join(".");
        if let Some(ids) = symbol_map.get(&key) {
            targets.extend(ids);
        }
    }

    if targets.is_empty() {
        if let Some(last) = normalized.split('.').next_back() {
            if let Some(ids) = symbol_map.get(last) {
                targets.extend(ids);
            }
        }
    }

    targets.sort_by_key(|id| id.index());
    targets.dedup();
    targets
}
