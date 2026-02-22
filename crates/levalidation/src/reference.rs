//! Reference integrity checking via legraphe

use crate::edit_change::EditChange;
use crate::ValidationError;
use crate::Location;
use legraphe::ProgramDependenceGraph;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Type of reference issue found
#[derive(Debug, Clone, PartialEq)]
pub enum ReferenceIssueType {
    /// Imported symbol doesn't exist
    BrokenImport {
        /// The symbol that couldn't be found
        symbol: String,
    },
    /// Used but not defined reference
    UndefinedReference {
        /// The undefined name
        name: String,
    },
    /// New cycle introduced in dependencies
    CyclicDependency {
        /// The cycle of dependencies
        cycle: Vec<String>,
    },
}

/// A reference issue found during validation
#[derive(Debug, Clone)]
pub struct ReferenceIssue {
    /// Type of reference issue
    pub issue_type: ReferenceIssueType,
    /// File path where the issue occurred
    pub file_path: PathBuf,
    /// Location in the file
    pub location: Location,
    /// Description of the issue
    pub description: String,
}

impl ReferenceIssue {
    /// Create a new reference issue
    pub fn new(
        issue_type: ReferenceIssueType,
        file_path: PathBuf,
        location: Location,
        description: String,
    ) -> Self {
        Self {
            issue_type,
            file_path,
            location,
            description,
        }
    }

    /// Create a broken import issue
    pub fn broken_import(symbol: String, file_path: PathBuf, location: Location) -> Self {
        Self {
            issue_type: ReferenceIssueType::BrokenImport {
                symbol: symbol.clone(),
            },
            file_path,
            location,
            description: format!("Import '{}' not found in project", symbol),
        }
    }

    /// Create an undefined reference issue
    pub fn undefined_reference(name: String, file_path: PathBuf, location: Location) -> Self {
        Self {
            issue_type: ReferenceIssueType::UndefinedReference {
                name: name.clone(),
            },
            file_path,
            location,
            description: format!("Undefined reference '{}'", name),
        }
    }

    /// Create a cyclic dependency issue
    pub fn cyclic_dependency(cycle: Vec<String>, file_path: PathBuf) -> Self {
        Self {
            issue_type: ReferenceIssueType::CyclicDependency {
                cycle: cycle.clone(),
            },
            file_path,
            location: Location { line: 1, column: 1 },
            description: format!("Cyclic dependency detected: {}", cycle.join(" -> ")),
        }
    }
}

/// Reference checker using PDG
#[derive(Clone)]
pub struct ReferenceChecker {
    /// PDG for reference checking
    pdg: Arc<ProgramDependenceGraph>,
}

impl ReferenceChecker {
    /// Create a new reference checker
    pub fn new(pdg: Arc<ProgramDependenceGraph>) -> Self {
        Self { pdg }
    }

    /// Check references for edit changes
    ///
    /// # Arguments
    /// * `changes` - Edit changes to check
    ///
    /// # Returns
    /// Vector of reference issues found
    pub fn check_references(&self, changes: &[EditChange]) -> Result<Vec<ReferenceIssue>, ValidationError> {
        let mut issues = Vec::new();

        for change in changes {
            // Extract imports from new content
            let imports = self.extract_imports(change);

            // Check each import against the PDG
            for import in imports {
                if !self.import_exists_in_pdg(&import) {
                    issues.push(ReferenceIssue::broken_import(
                        import,
                        change.file_path.clone(),
                        Location { line: 1, column: 1 },
                    ));
                }
            }

            // Check for undefined references
            let undefined = self.find_undefined_references(change);
            issues.extend(undefined);
        }

        // Check for new cycles
        issues.extend(self.check_for_cycles(changes)?);

        Ok(issues)
    }

    /// Extract imports from edit change content
    fn extract_imports(&self, change: &EditChange) -> Vec<String> {
        let mut imports = Vec::new();
        let lang = change.infer_language();

        match lang {
            "python" => {
                for line in change.new_content.lines() {
                    let line = line.trim();
                    if line.starts_with("import ") || line.starts_with("from ") {
                        // Extract the import path
                        if let Some(rest) = line.strip_prefix("import ") {
                            let import_path = rest.split(" as ").next().unwrap_or(rest).trim();
                            imports.push(import_path.to_string());
                        } else if let Some(rest) = line.strip_prefix("from ") {
                            let import_path = rest.split(" import ").next().unwrap_or(rest).trim();
                            imports.push(import_path.to_string());
                        }
                    }
                }
            }
            "javascript" | "typescript" => {
                for line in change.new_content.lines() {
                    let line = line.trim();
                    if line.contains("import ") && line.contains("from ") {
                        // Extract from '...' or from "..."
                        if let Some(start) = line.find("from ") {
                            let rest = &line[start + 5..];
                            let quote = rest.chars().next();
                            if let Some('"') | Some('\'') = quote {
                                if let Some(end) = rest[1..].find(quote.unwrap()) {
                                    imports.push(rest[1..end + 1].to_string());
                                }
                            }
                        }
                    } else if line.contains("require(") {
                        // Extract require('...') or require("...")
                        if let Some(start) = line.find("require(") {
                            let rest = &line[start + 8..]; // Skip "require("
                            if let Some(end) = rest.find(')') {
                                let inner = &rest[..end];
                                let inner = inner.trim();
                                if inner.starts_with('"') || inner.starts_with('\'') {
                                    imports.push(inner[1..inner.len() - 1].to_string());
                                }
                            }
                        }
                    }
                }
            }
            "rust" => {
                for line in change.new_content.lines() {
                    let line = line.trim();
                    if line.starts_with("use ") {
                        let import_path = line.trim_start_matches("use ")
                            .trim_end_matches(';')
                            .trim()
                            .to_string();
                        imports.push(import_path);
                    } else if line.starts_with("mod ") {
                        let mod_name = line.trim_start_matches("mod ")
                            .trim_end_matches(';')
                            .trim()
                            .to_string();
                        imports.push(mod_name);
                    }
                }
            }
            "go" => {
                for line in change.new_content.lines() {
                    let line = line.trim();
                    if line.starts_with("\"") && line.contains("\"") {
                        let import_path = line.trim_matches('"');
                        imports.push(import_path.to_string());
                    }
                }
            }
            _ => {
                // For other languages, use basic regex-like patterns
                // This is a simplified approach
            }
        }

        imports
    }

    /// Check if an import exists in the PDG
    fn import_exists_in_pdg(&self, import: &str) -> bool {
        // Check if the import exists as a module or symbol in the PDG
        let import_lower = import.to_lowercase();

        // Check if any node in the PDG matches the import
        for node_id in self.pdg.node_indices() {
            if let Some(node) = self.pdg.get_node(node_id) {
                let node_name_lower = node.name.to_lowercase();
                if node_name_lower.contains(&import_lower) || import_lower.contains(&node_name_lower) {
                    return true;
                }
            }
        }

        // Also check file paths
        for node_id in self.pdg.node_indices() {
            if let Some(node) = self.pdg.get_node(node_id) {
                let file_path_lower = node.file_path.to_lowercase();
                if file_path_lower.contains(&import_lower) {
                    return true;
                }
            }
        }

        false
    }

    /// Find undefined references in the edit change
    fn find_undefined_references(&self, change: &EditChange) -> Vec<ReferenceIssue> {
        let mut issues = Vec::new();
        let lang = change.infer_language();

        match lang {
            "python" => {
                // Find function calls that might be undefined
                for (line_num, line) in change.new_content.lines().enumerate() {
                    // Simple heuristic: look for function calls
                    let calls = self.extract_python_function_calls(line);
                    for call in calls {
                        if !self.symbol_exists_in_pdg(&call) {
                            issues.push(ReferenceIssue::undefined_reference(
                                call,
                                change.file_path.clone(),
                                Location {
                                    line: line_num + 1,
                                    column: 1,
                                },
                            ));
                        }
                    }
                }
            }
            _ => {
                // For other languages, we'd need more sophisticated analysis
            }
        }

        issues
    }

    /// Extract Python function calls from a line
    fn extract_python_function_calls(&self, line: &str) -> Vec<String> {
        let mut calls = Vec::new();

        // Simple regex-like extraction for function_name( patterns
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if chars[i].is_alphabetic() || chars[i] == '_' {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();

                // Check if followed by '('
                if i < chars.len() && chars[i] == '(' {
                    // Skip built-ins
                    if !self.is_python_builtin(&name) {
                        calls.push(name);
                    }
                }
            } else {
                i += 1;
            }
        }

        calls
    }

    /// Check if a name is a Python built-in
    fn is_python_builtin(&self, name: &str) -> bool {
        const BUILTINS: &[&str] = &[
            "print", "len", "str", "int", "float", "list", "dict", "set", "tuple",
            "range", "enumerate", "zip", "map", "filter", "sorted", "reversed",
            "sum", "min", "max", "abs", "all", "any", "bool", "type", "isinstance",
            "open", "input", "exit", "quit", "help", "dir", "vars", "id",
            "super", "self", "cls", "None", "True", "False", "await", "async",
            "if", "else", "elif", "for", "while", "def", "class", "return", "yield",
            "import", "from", "as", "with", "try", "except", "finally", "raise",
            "assert", "pass", "break", "continue", "and", "or", "not", "in", "is",
            "lambda", "global", "nonlocal", "del",
        ];
        BUILTINS.contains(&name)
    }

    /// Check if a symbol exists in the PDG
    fn symbol_exists_in_pdg(&self, symbol: &str) -> bool {
        self.pdg.find_by_symbol(symbol).is_some()
    }

    /// Check for cycles introduced by the changes
    fn check_for_cycles(&self, changes: &[EditChange]) -> Result<Vec<ReferenceIssue>, ValidationError> {
        let mut issues = Vec::new();

        // Build a dependency graph from the changes
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut rec_stack: HashSet<String> = HashSet::new();

        for change in changes {
            let file_name = change.file_path.to_string_lossy().to_string();
            let imports = self.extract_imports(change);

            deps.insert(file_name.clone(), imports);
        }

        // Check for cycles using DFS
        for node in deps.keys() {
            if !visited.contains(node) {
                if let Some(cycle) = self.detect_cycle(node, &deps, &mut visited, &mut rec_stack) {
                    issues.push(ReferenceIssue::cyclic_dependency(
                        cycle,
                        PathBuf::from(node),
                    ));
                }
            }
        }

        Ok(issues)
    }

    /// Detect cycle using DFS
    fn detect_cycle(
        &self,
        node: &str,
        deps: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        visited.insert(node.to_string());
        rec_stack.insert(node.to_string());

        if let Some(neighbors) = deps.get(node) {
            for neighbor in neighbors {
                if !visited.contains(neighbor) {
                    if let Some(cycle) = self.detect_cycle(neighbor, deps, visited, rec_stack) {
                        let mut result = vec![node.to_string()];
                        result.extend(cycle);
                        return Some(result);
                    }
                } else if rec_stack.contains(neighbor) {
                    // Found a cycle
                    return Some(vec![neighbor.to_string(), node.to_string()]);
                }
            }
        }

        rec_stack.remove(node);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_reference_issue_broken_import() {
        let issue = ReferenceIssue::broken_import(
            "missing_module".to_string(),
            PathBuf::from("test.py"),
            Location { line: 1, column: 1 },
        );
        assert!(matches!(issue.issue_type, ReferenceIssueType::BrokenImport { .. }));
        assert_eq!(issue.file_path, PathBuf::from("test.py"));
    }

    #[test]
    fn test_reference_issue_undefined_reference() {
        let issue = ReferenceIssue::undefined_reference(
            "undefined_func".to_string(),
            PathBuf::from("test.py"),
            Location { line: 5, column: 10 },
        );
        assert!(matches!(
            issue.issue_type,
            ReferenceIssueType::UndefinedReference { .. }
        ));
    }

    #[test]
    fn test_reference_issue_cyclic_dependency() {
        let cycle = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        let issue = ReferenceIssue::cyclic_dependency(cycle, PathBuf::from("test.py"));
        assert!(matches!(
            issue.issue_type,
            ReferenceIssueType::CyclicDependency { .. }
        ));
        assert!(issue.description.contains("a -> b -> a"));
    }

    #[test]
    fn test_reference_checker_new() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let _checker = ReferenceChecker::new(pdg);
        // Just verify it was created
        assert!(true);
    }

    #[test]
    fn test_extract_imports_python() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let checker = ReferenceChecker::new(pdg);

        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "import os\nimport sys\nfrom collections import defaultdict\n".to_string(),
        );

        let imports = checker.extract_imports(&change);
        assert_eq!(imports, vec!["os", "sys", "collections"]);
    }

    #[test]
    fn test_extract_imports_javascript() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let checker = ReferenceChecker::new(pdg);

        let change = EditChange::new(
            PathBuf::from("test.js"),
            String::new(),
            "import { foo } from 'bar';\nconst baz = require('qux');\n".to_string(),
        );

        let imports = checker.extract_imports(&change);
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn test_extract_imports_rust() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let checker = ReferenceChecker::new(pdg);

        let change = EditChange::new(
            PathBuf::from("test.rs"),
            String::new(),
            "use std::collections::HashMap;\nmod my_module;\n".to_string(),
        );

        let imports = checker.extract_imports(&change);
        assert_eq!(imports, vec!["std::collections::HashMap", "my_module"]);
    }

    #[test]
    fn test_is_python_builtin() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let checker = ReferenceChecker::new(pdg);

        assert!(checker.is_python_builtin("print"));
        assert!(checker.is_python_builtin("len"));
        assert!(checker.is_python_builtin("if"));
        assert!(!checker.is_python_builtin("my_function"));
    }

    #[test]
    fn test_extract_python_function_calls() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let checker = ReferenceChecker::new(pdg);

        let calls = checker.extract_python_function_calls("x = foo() + bar()");
        assert_eq!(calls, vec!["foo", "bar"]);

        let calls = checker.extract_python_function_calls("print('hello')");
        assert!(calls.is_empty()); // print is a builtin

        let calls = checker.extract_python_function_calls("my_func()");
        assert_eq!(calls, vec!["my_func"]);
    }

    #[test]
    fn test_reference_issue_type_equality() {
        assert_eq!(
            ReferenceIssueType::BrokenImport {
                symbol: "foo".to_string()
            },
            ReferenceIssueType::BrokenImport {
                symbol: "foo".to_string()
            }
        );
        assert_ne!(
            ReferenceIssueType::BrokenImport {
                symbol: "foo".to_string()
            },
            ReferenceIssueType::UndefinedReference {
                name: "foo".to_string()
            }
        );
    }
}
