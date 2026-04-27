use super::protocol::JsonRpcError;
use crate::edit::EditChange;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Helper to extract required string argument
pub(crate) fn extract_string(args: &Value, key: &str) -> Result<String, JsonRpcError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("Missing required argument: {}", key),
                format!("Add \"{}\": \"<value>\" to arguments", key),
            )
        })
}

/// Helper to extract usize argument with default.
pub(crate) fn extract_usize(args: &Value, key: &str, default: usize) -> Result<usize, JsonRpcError> {
    match args.get(key) {
        Some(Value::Number(n)) => Ok(n.as_u64().map(|v| v as usize).unwrap_or(default)),
        Some(Value::String(s)) => s.trim().parse::<usize>().or(Ok(default)),
        _ => Ok(default),
    }
}

/// Helper to extract bool argument with default.
pub(crate) fn extract_bool(args: &Value, key: &str, default: bool) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => default,
        },
        Some(Value::Number(n)) => n.as_u64().map(|v| v != 0).unwrap_or(default),
        _ => default,
    }
}

/// Validate that a file path resides within the project root.
pub(crate) fn validate_file_within_project(
    file_path: &str,
    project_root: &std::path::Path,
) -> Result<PathBuf, JsonRpcError> {
    let canonical = std::path::Path::new(file_path)
        .canonicalize()
        .map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot resolve file path '{}': {}", file_path, e))
        })?;
    if !canonical.starts_with(project_root) {
        return Err(JsonRpcError::invalid_params(format!(
            "File '{}' is outside the project boundary '{}'",
            file_path,
            project_root.display()
        )));
    }
    Ok(canonical)
}

/// Format a NodeType as a human-readable string.
pub(crate) fn node_type_str(nt: &crate::graph::pdg::NodeType) -> &'static str {
    match nt {
        crate::graph::pdg::NodeType::Function => "function",
        crate::graph::pdg::NodeType::Class => "class",
        crate::graph::pdg::NodeType::Method => "method",
        crate::graph::pdg::NodeType::Variable => "variable",
        crate::graph::pdg::NodeType::Module => "module",
        crate::graph::pdg::NodeType::External => "external",
    }
}

/// Resolve and normalize a scope path for consistent filtering.
pub(crate) fn resolve_scope(args: &Value, project_root: &Path) -> Result<Option<String>, JsonRpcError> {
    let raw = match args.get("scope").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(None),
    };

    let path = Path::new(raw);

    let resolved = if path.is_relative() {
        project_root.join(path)
    } else {
        path.to_path_buf()
    };

    let canonical = resolved.canonicalize().map_err(|e| {
        JsonRpcError::invalid_params_with_suggestion(
            format!("Cannot resolve scope path '{}': {}", raw, e),
            format!(
                "Use an absolute path or a path relative to the project root: {}",
                project_root.display()
            ),
        )
    })?;

    let mut s = canonical.to_string_lossy().to_string();
    if canonical.is_dir() && !s.ends_with(std::path::MAIN_SEPARATOR) && !s.ends_with('/') {
        s.push(std::path::MAIN_SEPARATOR);
    }

    Ok(Some(s))
}

/// Attach meta information to tool responses about index staleness and context.
pub(crate) fn wrap_with_meta(mut result: Value, index: &crate::cli::leindex::LeIndex) -> Value {
    let stale = index.is_stale_fast();
    if let Some(obj) = result.as_object_mut() {
        if stale {
            obj.insert(
                "_warning".to_string(),
                Value::String(
                    "Index may be stale. Call leindex_index with force_reindex=true for fresh results."
                        .to_string(),
                ),
            );
        }
    }
    result
}

/// Read a source snippet from disk using the node's byte_range.
pub(crate) fn read_source_snippet(file_path: &str, byte_range: (usize, usize)) -> Option<String> {
    if byte_range.1 <= byte_range.0 {
        return None;
    }
    let bytes = std::fs::read(file_path).ok()?;
    let start = byte_range.0.min(bytes.len());
    let end = byte_range.1.min(bytes.len());
    if start >= end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

/// Convert a byte range to a 1-indexed line range.
pub(crate) fn byte_range_to_line_range(content: &str, byte_range: (usize, usize)) -> (usize, usize) {
    let (start, end) = byte_range;
    let bytes = content.as_bytes();
    let mut line = 1usize;
    let mut start_line = 1usize;
    let mut end_line = 1usize;
    let mut found_start = false;

    for (idx, b) in bytes.iter().enumerate() {
        if idx == start {
            start_line = line;
            found_start = true;
        }
        if idx >= end {
            end_line = line;
            break;
        }
        if *b == b'\n' {
            line += 1;
        }
    }
    if !found_start {
        start_line = line;
    }
    if end >= bytes.len() {
        end_line = line;
    }
    (start_line, end_line.max(start_line))
}

/// Collect the NodeIds of all nodes that have a direct edge pointing *to* `target_id`.
pub(crate) fn get_direct_callers(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    target_id: crate::graph::pdg::NodeId,
) -> Vec<crate::graph::pdg::NodeId> {
    pdg.predecessors(target_id)
}

/// Simple glob matching for include/exclude patterns.
pub(crate) fn glob_match(path: &str, pattern: &str) -> bool {
    if pattern.starts_with("*.") {
        let ext = &pattern[1..];
        path.ends_with(ext)
    } else if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            path.contains(parts[0]) && path.ends_with(parts[1])
        } else {
            path.contains(pattern)
        }
    } else {
        path.contains(pattern)
    }
}

/// Parse a JSON `changes` array into a Vec<EditChange>.
pub(crate) fn parse_edit_changes(
    changes_val: &Value,
    content: Option<&str>,
) -> Result<Vec<EditChange>, JsonRpcError> {
    let arr = changes_val
        .as_array()
        .ok_or_else(|| JsonRpcError::invalid_params("'changes' must be an array"))?;

    let mut result = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let change_type = item.get("type").and_then(|v| v.as_str())
            .or_else(|| {
                if item.get("old_text").is_some() || item.get("old_str").is_some() {
                    Some("replace_text")
                } else if item.get("old_name").is_some() && item.get("new_name").is_some() {
                    Some("rename_symbol")
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                JsonRpcError::invalid_params(format!("changes[{}]: missing 'type' — use 'replace_text' or 'rename_symbol', or provide old_text+new_text", i))
            })?;

        let change = match change_type {
            "replace_text" => {
                let old_text = item
                    .get("old_text")
                    .or_else(|| item.get("old_str"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let new_text = item
                    .get("new_text")
                    .or_else(|| item.get("new_str"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_text'", i))
                    })?;

                let has_explicit_start = item.get("start_byte").is_some();
                let has_explicit_end = item.get("end_byte").is_some();

                if has_explicit_start || has_explicit_end {
                    let start = item.get("start_byte").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let end = item.get("end_byte").and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(start + old_text.len());
                    EditChange::ReplaceText {
                        start,
                        end,
                        new_text: new_text.to_owned(),
                    }
                } else if !old_text.is_empty() {
                    if let Some(content) = content {
                        if let Some(pos) = content.find(old_text) {
                            EditChange::ReplaceText {
                                start: pos,
                                end: pos + old_text.len(),
                                new_text: new_text.to_owned(),
                            }
                        } else if let Some((pos, matched_len)) = find_normalised_whitespace(content, old_text) {
                            EditChange::ReplaceText {
                                start: pos,
                                end: pos + matched_len,
                                new_text: new_text.to_owned(),
                            }
                        } else {
                            let preview = if old_text.len() > 60 { format!("{}...", &old_text[..60]) } else { old_text.to_string() };
                            return Err(JsonRpcError::invalid_params_with_suggestion(
                                format!("changes[{}]: old_text not found in file content: '{}'", i, preview),
                                "Ensure old_text exactly matches the source. Whitespace-normalised matching is attempted automatically.",
                            ));
                        }
                    } else {
                        let start = 0usize;
                        let end = old_text.len();
                        EditChange::ReplaceText { start, end, new_text: new_text.to_owned() }
                    }
                } else {
                    return Err(JsonRpcError::invalid_params(format!(
                        "changes[{}]: replace_text requires either 'start_byte'/'end_byte' or non-empty 'old_text'", i
                    )));
                }
            }
            "rename_symbol" => {
                let old_name = item.get("old_name").and_then(|v| v.as_str()).ok_or_else(|| JsonRpcError::invalid_params(format!("changes[{}]: missing 'old_name'", i)))?;
                let new_name = item.get("new_name").and_then(|v| v.as_str()).ok_or_else(|| JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_name'", i)))?;
                EditChange::RenameSymbol { old_name: old_name.to_owned(), new_name: new_name.to_owned() }
            }
            other => return Err(JsonRpcError::invalid_params(format!("changes[{}]: unknown type '{}'", i, other))),
        };
        result.push(change);
    }
    Ok(result)
}

/// Apply a Vec<EditChange> to content in memory and return the modified string.
pub(crate) fn apply_changes_in_memory(content: &str, changes: &[EditChange]) -> Result<String, JsonRpcError> {
    let mut replace_changes: Vec<&EditChange> = Vec::new();
    let mut other_changes: Vec<&EditChange> = Vec::new();
    for change in changes {
        match change {
            EditChange::ReplaceText { .. } => replace_changes.push(change),
            _ => other_changes.push(change),
        }
    }

    replace_changes.sort_by(|a, b| {
        let a_start = if let EditChange::ReplaceText { start, .. } = a { *start } else { 0 };
        let b_start = if let EditChange::ReplaceText { start, .. } = b { *start } else { 0 };
        b_start.cmp(&a_start)
    });

    let mut modified = content.to_owned();

    for change in &replace_changes {
        if let EditChange::ReplaceText { start, end, new_text } = change {
            let bytes = modified.as_bytes();
            let s = (*start).min(bytes.len());
            let e = (*end).min(bytes.len());
            modified = format!("{}{}{}", &modified[..s], new_text, &modified[e..]);
        }
    }

    for change in &other_changes {
        modified = match change {
            EditChange::RenameSymbol { old_name, new_name } => replace_whole_word(&modified, old_name, new_name),
            _ => modified,
        };
    }

    Ok(modified)
}

/// Replace `old` with `new` only at word boundaries.
pub(crate) fn replace_whole_word(content: &str, old: &str, new: &str) -> String {
    if old.is_empty() { return content.to_owned(); }
    fn is_word_char(c: char) -> bool { c.is_alphanumeric() || c == '_' }
    let mut result = String::with_capacity(content.len());
    let mut last_match_end = 0usize;
    for (start, matched) in content.match_indices(old) {
        let end = start + matched.len();
        let before_ok = start == 0 || content[..start].chars().last().map(|c| !is_word_char(c)).unwrap_or(true);
        let after_ok = end == content.len() || content[end..].chars().next().map(|c| !is_word_char(c)).unwrap_or(true);
        if before_ok && after_ok {
            result.push_str(&content[last_match_end..start]);
            result.push_str(new);
            last_match_end = end;
        }
    }
    result.push_str(&content[last_match_end..]);
    result
}

/// Normalise whitespace in a string.
pub(crate) fn normalise_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() { out.push(' '); }
            in_ws = true;
        } else {
            in_ws = false;
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

/// Find `needle` in `haystack` using whitespace-normalised matching.
pub(crate) fn find_normalised_whitespace(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let norm_needle = normalise_ws(needle);
    if norm_needle.is_empty() { return None; }
    let lines: Vec<&str> = haystack.lines().collect();
    for start_line in 0..lines.len() {
        let mut window = String::new();
        let mut raw_start_byte: Option<usize> = None;
        for end_line in start_line..lines.len().min(start_line + needle.lines().count() + 5) {
            if !window.is_empty() { window.push(' '); }
            window.push_str(lines[end_line].trim());
            let norm_window = normalise_ws(&window);
            if norm_window.find(&norm_needle).is_some() {
                let byte_start = if let Some(s) = raw_start_byte { s } else {
                    let mut offset = 0;
                    for l in 0..start_line { offset += lines[l].len() + 1; }
                    offset
                };
                let mut byte_end = byte_start;
                for l in start_line..=end_line { byte_end += lines[l].len() + 1; }
                return Some((byte_start, byte_end.min(haystack.len()) - byte_start));
            }
            if raw_start_byte.is_none() {
                let mut offset = 0;
                for l in 0..start_line { offset += lines[l].len() + 1; }
                raw_start_byte = Some(offset);
            }
        }
    }
    None
}

/// Generate a unified diff between two strings.
pub(crate) fn make_diff(original: &str, modified: &str, file_path: &str) -> String {
    let patch = diffy::create_patch(original, modified);
    let patch_str = patch.to_string();
    if patch_str.is_empty() {
        format!("--- {}\n+++ {}\n(no changes)\n", file_path, file_path)
    } else {
        format!("--- {}\n+++ {}\n{}", file_path, file_path, patch_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;
    use crate::cli::leindex::LeIndex;
    use crate::cli::registry::ProjectRegistry;

    #[allow(dead_code)]
    fn test_registry_for(path: &std::path::Path) -> Arc<ProjectRegistry> {
        let leindex = LeIndex::new(path).expect("leindex");
        Arc::new(ProjectRegistry::with_initial_project(5, leindex))
    }

    #[test]
    fn test_extract_string() {
        let args = serde_json::json!({"query": "test"});
        assert_eq!(extract_string(&args, "query").unwrap(), "test");
        assert!(extract_string(&args, "missing").is_err());
    }

    #[test]
    fn test_extract_usize() {
        let args = serde_json::json!({"top_k": 20});
        assert_eq!(extract_usize(&args, "top_k", 10).unwrap(), 20);
        assert_eq!(extract_usize(&args, "missing", 10).unwrap(), 10);
    }

    #[test]
    fn test_extract_bool_native_bool() {
        let args = serde_json::json!({"flag": true, "off": false});
        assert_eq!(extract_bool(&args, "flag", false), true);
        assert_eq!(extract_bool(&args, "off", true), false);
    }

    #[test]
    fn test_extract_bool_string_coercion() {
        let args = serde_json::json!({
            "a": "true", "b": "false", "c": "1", "d": "0", "e": "yes", "f": "no",
            "g": "TRUE", "h": "False"
        });
        assert_eq!(extract_bool(&args, "a", false), true);
        assert_eq!(extract_bool(&args, "b", true), false);
        assert_eq!(extract_bool(&args, "c", false), true);
        assert_eq!(extract_bool(&args, "d", true), false);
        assert_eq!(extract_bool(&args, "e", false), true);
        assert_eq!(extract_bool(&args, "f", true), false);
        assert_eq!(extract_bool(&args, "g", false), true);
        assert_eq!(extract_bool(&args, "h", true), false);
    }

    #[test]
    fn test_extract_bool_number_coercion() {
        let args = serde_json::json!({"one": 1, "zero": 0, "big": 42});
        assert_eq!(extract_bool(&args, "one", false), true);
        assert_eq!(extract_bool(&args, "zero", true), false);
        assert_eq!(extract_bool(&args, "big", false), true);
    }

    #[test]
    fn test_extract_bool_missing_uses_default() {
        let args = serde_json::json!({});
        assert_eq!(extract_bool(&args, "absent", true), true);
        assert_eq!(extract_bool(&args, "absent", false), false);
    }

    #[test]
    fn test_validate_file_within_project_ok() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();
        let result = validate_file_within_project(file.to_str().unwrap(), dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_file_outside_project_fails() {
        let dir = tempdir().unwrap();
        let result = validate_file_within_project("/etc/passwd", dir.path());
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("outside the project boundary"));
    }

    #[test]
    fn test_node_type_str() {
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Function), "function");
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Class), "class");
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Method), "method");
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Variable), "variable");
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Module), "module");
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::External), "external");
    }

    #[test]
    fn test_read_source_snippet_empty_range() {
        assert!(read_source_snippet("/nonexistent/path", (0, 0)).is_none());
    }

    #[test]
    fn test_read_source_snippet_from_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, b"pub fn hello() {}").unwrap();
        let path = file.to_str().unwrap();
        let snippet = read_source_snippet(path, (0, 17));
        assert!(snippet.is_some());
        assert_eq!(snippet.unwrap(), "pub fn hello() {}");
    }

    #[test]
    fn test_get_direct_callers_empty_pdg() {
        let pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let node = crate::graph::pdg::Node {
            id: "test".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "test".into(),
            file_path: "test.rs".into(),
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".into(),
        };
        let mut pdg = pdg;
        let nid = pdg.add_node(node);
        let callers = get_direct_callers(&pdg, nid);
        assert!(callers.is_empty());
    }

    #[test]
    fn test_get_direct_callers_with_edge() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let cid = pdg.add_node(crate::graph::pdg::Node { id: "caller".into(), node_type: crate::graph::pdg::NodeType::Function, name: "caller".into(), file_path: "a.rs".into(), byte_range: (0, 10), complexity: 1, language: "rust".into() });
        let did = pdg.add_node(crate::graph::pdg::Node { id: "callee".into(), node_type: crate::graph::pdg::NodeType::Function, name: "callee".into(), file_path: "b.rs".into(), byte_range: (0, 10), complexity: 1, language: "rust".into() });
        pdg.add_call_graph_edges(vec![(cid, did)]);
        let callers = get_direct_callers(&pdg, did);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0], cid);
    }

    #[test]
    fn test_replace_whole_word_basic() {
        assert_eq!(replace_whole_word("foo bar baz", "bar", "qux"), "foo qux baz");
        assert_eq!(replace_whole_word("foobar baz", "bar", "qux"), "foobar baz");
        assert_eq!(replace_whole_word("bar_foo", "bar", "qux"), "bar_foo");
        assert_eq!(replace_whole_word("bar", "bar", "qux"), "qux");
    }

    #[test]
    fn test_parse_edit_changes_text_search_mode() {
        let content = "fn hello() {\n    println!(\"Hello\");\n}";
        let changes_json = serde_json::json!([{"type": "replace_text", "old_text": "println!(\"Hello\")", "new_text": "println!(\"Goodbye\")"}]);
        let changes = parse_edit_changes(&changes_json, Some(content)).unwrap();
        assert_eq!(changes.len(), 1);
        if let EditChange::ReplaceText { start, end, new_text } = &changes[0] {
            assert_eq!(*start, content.find("println!(\"Hello\")").unwrap());
            assert_eq!(*end, *start + "println!(\"Hello\")".len());
            assert_eq!(new_text, "println!(\"Goodbye\")");
        } else { panic!("Expected ReplaceText"); }
    }

    #[test]
    fn test_apply_changes_in_memory_text_search_integration() {
        let content = "def health_check(self):\n    return True\n\ndef other():\n    pass";
        let changes_json = serde_json::json!([{"type": "replace_text", "old_text": "def health_check(self):\n    return True", "new_text": "def health_status(self):\n    return True"}]);
        let changes = parse_edit_changes(&changes_json, Some(content)).unwrap();
        let modified = apply_changes_in_memory(content, &changes).unwrap();
        assert!(modified.contains("def health_status(self):"));
        assert!(modified.contains("def other():"));
    }
}
