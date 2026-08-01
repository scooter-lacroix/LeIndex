use super::protocol::JsonRpcError;
use crate::cli::leindex::{ComponentStatus, IndexHealth, SOURCE_FILE_EXTENSIONS};
use crate::edit::{EditChange, replace_whole_word};
use crate::skip_dirs::SKIP_DIRS;
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
pub(crate) fn extract_usize(
    args: &Value,
    key: &str,
    default: usize,
) -> Result<usize, JsonRpcError> {
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
    let candidate = if std::path::Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        project_root.join(file_path)
    };
    let canonical = candidate.canonicalize().map_err(|e| {
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
        crate::graph::pdg::NodeType::FileSummary => "file_summary",
    }
}

/// Resolve and normalize a scope path for consistent filtering.
pub(crate) fn resolve_scope(
    args: &Value,
    project_root: &Path,
) -> Result<Option<String>, JsonRpcError> {
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
pub(crate) fn wrap_with_meta(result: Value, index: &crate::cli::leindex::LeIndex) -> Value {
    // Git porcelain is the cheapest authoritative live delta for a worktree;
    // use it once per response to avoid turning a persisted health snapshot
    // into a stale Boolean that survives an editor save.
    let live_dirty = crate::cli::git::status(index.project_path())
        .ok()
        .map(|status| {
            status.modified.len()
                + status.staged.len()
                + status.untracked.len()
                + status.deleted.len()
        })
        .unwrap_or(0);
    wrap_live_with_meta_dirty(result, index.project_path(), live_dirty)
}

/// Attach the same compact freshness badge to a live-only response. This path
/// reads the tiny health snapshot but never constructs a `LeIndex`.
pub(crate) fn wrap_live_with_meta(result: Value, project_root: &std::path::Path) -> Value {
    let live_dirty = crate::cli::git::status(project_root)
        .ok()
        .map(|status| {
            status.modified.len()
                + status.staged.len()
                + status.untracked.len()
                + status.deleted.len()
        })
        .unwrap_or(0);
    wrap_live_with_meta_dirty(result, project_root, live_dirty)
}

/// Determine whether the index is genuinely stale by checking if dirty files
/// affect actual indexed source files.
///
/// Returns `false` when:
/// - The index health is `Fresh` and the tree OID matches the current git tree
/// - Dirty files are only untracked files without source extensions
/// - Dirty files are inside skipped directories (`.leindex/`, `target/`, etc.)
///
/// Returns `true` only when dirty files include tracked source files that
/// differ from what was indexed.
pub(crate) fn is_index_genuinely_stale(health: &Option<IndexHealth>, project_root: &Path) -> bool {
    let Some(health) = health else {
        // No health info at all means we can't confirm freshness.
        return true;
    };

    // If health status is explicitly Stale/Partial/Failed, trust it.
    if matches!(
        health.status,
        ComponentStatus::Stale | ComponentStatus::Partial | ComponentStatus::Failed
    ) {
        return true;
    }

    // If the index is Fresh and the tree OID matches the current git HEAD tree,
    // the index is current even if there are uncommitted changes to unrelated
    // files.
    if health.status == ComponentStatus::Fresh {
        if let Some(indexed_tree_oid) = &health.tree_oid {
            if let Ok(Some(current_tree_oid)) = crate::cli::git::tree_oid(project_root) {
                if indexed_tree_oid == &current_tree_oid {
                    return false;
                }
            }
        }
    }

    // Check whether the dirty files actually affect source files that were
    // previously indexed. Get the git status to inspect individual paths.
    let git_status = match crate::cli::git::status(project_root) {
        Ok(status) => status,
        Err(_) => {
            // If we can't get git status, fall back to the dirty count from
            // health.
            return health.dirty_file_count > 0;
        }
    };

    // Collect all dirty paths (modified, staged, deleted, conflicted).
    // Untracked files are checked separately - they only count as stale if
    // they have source extensions.
    let dirty_tracked: Vec<&PathBuf> = git_status
        .modified
        .iter()
        .chain(&git_status.staged)
        .chain(&git_status.deleted)
        .chain(&git_status.conflicted)
        .collect();

    // If any tracked file is dirty, the index is genuinely stale (these are
    // files that were potentially indexed and have changed).
    if !dirty_tracked.is_empty() {
        // But only count files with source extensions or that are known
        // indexed files.
        let has_source_changes = dirty_tracked
            .iter()
            .any(|path| is_source_file(path) && !is_in_skip_dir(path));
        if has_source_changes {
            return true;
        }
    }

    // Check untracked files: only stale if they have source extensions and
    // are not in skip directories.
    git_status
        .untracked
        .iter()
        .any(|path| is_source_file(path) && !is_in_skip_dir(path))
}

/// Check if a path has a source file extension.
fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| SOURCE_FILE_EXTENSIONS.contains(&ext))
}

/// Check if a path resides inside a skip directory.
fn is_in_skip_dir(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
    })
}

pub(crate) fn wrap_live_with_meta_dirty(
    mut result: Value,
    project_root: &std::path::Path,
    live_dirty: usize,
) -> Value {
    let storage_path = crate::cli::leindex::resolve_existing_storage_path(project_root)
        .unwrap_or_else(|| project_root.join(".leindex"));
    let health = crate::cli::index_freshness::load_health(&storage_path);
    let freshness = health
        .as_ref()
        .map(|health| {
            let age_ms = health.indexed_at_unix_ms.and_then(|indexed_at| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                Some(now.saturating_sub(indexed_at))
            });
            serde_json::json!({
                "generation": health.generation,
                "status": health.status,
                "phase": health.phase,
                "head_oid": health.head_oid,
                "tree_oid": health.tree_oid,
                "indexed_file_count": health.indexed_file_count,
                "dirty_file_count": health.dirty_file_count.max(live_dirty),
                "changed_unindexed_count": health.changed_unindexed_count.max(live_dirty),
                "age_ms": age_ms,
                "last_failure_phase": health.last_failure_phase,
                "last_failure": health.last_failure,
            })
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "generation": Value::Null,
                "status": "not_loaded",
                "phase": Value::Null,
                "head_oid": Value::Null,
                "tree_oid": Value::Null,
                "indexed_file_count": 0,
                "dirty_file_count": live_dirty,
                "changed_unindexed_count": live_dirty,
                "age_ms": Value::Null,
                "last_failure_phase": Value::Null,
                "last_failure": Value::Null,
            })
        });
    if let Some(obj) = result.as_object_mut() {
        let stale = is_index_genuinely_stale(&health, project_root);
        {
            let mut freshness = freshness;
            if stale {
                if let Some(freshness_obj) = freshness.as_object_mut() {
                    freshness_obj.insert(
                        "warning".to_string(),
                        Value::String(
                            if health.is_none() {
                                "No indexed generation is loaded; exact live results remain usable."
                            } else {
                                "Index may be stale; run leindex.index with force_reindex=true to refresh."
                            }
                            .to_string(),
                        ),
                    );
                }
            }
            let meta = obj
                .entry("_meta".to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(meta_obj) = meta.as_object_mut() {
                meta_obj.insert("freshness".to_string(), freshness);
            }
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
    if byte_range.0 > bytes.len() || byte_range.1 > bytes.len() {
        return None;
    }
    let start = byte_range.0;
    let end = byte_range.1;
    if start >= end {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes[start..end]).into_owned())
}

/// Convert a byte range to a 1-indexed line range.
pub(crate) fn byte_range_to_line_range(
    content: &str,
    byte_range: (usize, usize),
) -> (usize, usize) {
    let (start, end) = byte_range;
    let bytes = content.as_bytes();
    if start > bytes.len() || end > bytes.len() || end < start {
        return (0, 0);
    }
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

fn edit_change_type(item: &Value, index: usize) -> Result<&str, JsonRpcError> {
    item.get("type")
        .and_then(Value::as_str)
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
            JsonRpcError::invalid_params(format!(
                "changes[{}]: missing 'type' — use 'replace_text' or 'rename_symbol', or provide old_text+new_text",
                index
            ))
        })
}

fn missing_old_text(index: usize, old_text: &str) -> JsonRpcError {
    let preview = if old_text.len() > 60 {
        let safe_end = old_text
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|&index| index <= 60)
            .last()
            .unwrap_or(0);
        format!("{}...", &old_text[..safe_end])
    } else {
        old_text.to_string()
    };
    JsonRpcError::invalid_params_with_suggestion(
        format!(
            "changes[{}]: old_text not found in file content: '{}'",
            index, preview
        ),
        "Ensure old_text exactly matches the source. Whitespace-normalised matching is attempted automatically.",
    )
}

fn parse_replace_change(
    item: &Value,
    index: usize,
    content: Option<&str>,
) -> Result<EditChange, JsonRpcError> {
    let old_text = item
        .get("old_text")
        .or_else(|| item.get("old_str"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let new_text = item
        .get("new_text")
        .or_else(|| item.get("new_str"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_text'", index))
        })?;
    let has_explicit_range = item.get("start_byte").is_some() || item.get("end_byte").is_some();
    if has_explicit_range {
        let start = item.get("start_byte").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = item
            .get("end_byte")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(start + old_text.len());
        return Ok(EditChange::ReplaceText {
            start,
            end,
            new_text: new_text.to_owned(),
        });
    }
    if old_text.is_empty() {
        return Err(JsonRpcError::invalid_params(format!(
            "changes[{}]: replace_text requires either 'start_byte'/'end_byte' or non-empty 'old_text'",
            index
        )));
    }
    let Some(content) = content else {
        return Ok(EditChange::ReplaceText {
            start: 0,
            end: old_text.len(),
            new_text: new_text.to_owned(),
        });
    };
    let (start, length) = if let Some(position) = content.find(old_text) {
        (position, old_text.len())
    } else if let Some((position, length)) = find_normalised_whitespace(content, old_text) {
        (position, length)
    } else {
        return Err(missing_old_text(index, old_text));
    };
    Ok(EditChange::ReplaceText {
        start,
        end: start + length,
        new_text: new_text.to_owned(),
    })
}

fn parse_rename_change(item: &Value, index: usize) -> Result<EditChange, JsonRpcError> {
    let old_name = item
        .get("old_name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("changes[{}]: missing 'old_name'", index))
        })?;
    let new_name = item
        .get("new_name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JsonRpcError::invalid_params(format!("changes[{}]: missing 'new_name'", index))
        })?;
    Ok(EditChange::RenameSymbol {
        old_name: old_name.to_owned(),
        new_name: new_name.to_owned(),
    })
}

/// Parse a JSON `changes` array into a Vec<EditChange>.
pub(crate) fn parse_edit_changes(
    changes_val: &Value,
    content: Option<&str>,
) -> Result<Vec<EditChange>, JsonRpcError> {
    let changes = changes_val
        .as_array()
        .ok_or_else(|| JsonRpcError::invalid_params("'changes' must be an array"))?;
    changes
        .iter()
        .enumerate()
        .map(|(index, item)| match edit_change_type(item, index)? {
            "replace_text" => parse_replace_change(item, index, content),
            "rename_symbol" => parse_rename_change(item, index),
            other => Err(JsonRpcError::invalid_params(format!(
                "changes[{}]: unknown type '{}'",
                index, other
            ))),
        })
        .collect()
}

/// Apply a Vec<EditChange> to content in memory and return the modified string.
pub(crate) fn apply_changes_in_memory(
    content: &str,
    changes: &[EditChange],
) -> Result<String, JsonRpcError> {
    let mut replace_changes: Vec<&EditChange> = Vec::new();
    let mut other_changes: Vec<&EditChange> = Vec::new();
    for change in changes {
        match change {
            EditChange::ReplaceText { .. } => replace_changes.push(change),
            _ => other_changes.push(change),
        }
    }

    replace_changes.sort_by(|a, b| {
        let a_start = if let EditChange::ReplaceText { start, .. } = a {
            *start
        } else {
            0
        };
        let b_start = if let EditChange::ReplaceText { start, .. } = b {
            *start
        } else {
            0
        };
        b_start.cmp(&a_start)
    });

    let mut modified = content.to_owned();

    for change in &replace_changes {
        if let EditChange::ReplaceText {
            start,
            end,
            new_text,
        } = change
        {
            let bytes = modified.as_bytes();
            let s = (*start).min(bytes.len());
            let e = (*end).min(bytes.len());

            // Validate UTF-8 character boundaries
            if !modified.is_char_boundary(s) {
                return Err(JsonRpcError::invalid_params(format!(
                    "start_byte {} is not on a valid UTF-8 character boundary",
                    s
                )));
            }
            if !modified.is_char_boundary(e) {
                return Err(JsonRpcError::invalid_params(format!(
                    "end_byte {} is not on a valid UTF-8 character boundary",
                    e
                )));
            }
            // Validate range ordering
            if s > e {
                return Err(JsonRpcError::invalid_params(format!(
                    "start_byte {} must be <= end_byte {}",
                    s, e
                )));
            }

            modified.replace_range(s..e, new_text);
        }
    }

    for change in &other_changes {
        modified = match change {
            EditChange::RenameSymbol { old_name, new_name } => {
                replace_whole_word(&modified, old_name, new_name)
            }
            _ => modified,
        };
    }

    Ok(modified)
}

/// Normalise whitespace in a string.
pub(crate) fn normalise_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = true;
        } else {
            in_ws = false;
            out.push(ch);
        }
    }
    out.trim_end().to_string()
}

pub(crate) fn normalise_ws_with_spans(s: &str) -> (String, Vec<(usize, usize)>) {
    let mut chars: Vec<char> = Vec::with_capacity(s.len());
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(s.len());
    let mut seen_non_ws = false;
    let mut in_ws = false;

    for (idx, ch) in s.char_indices() {
        if ch.is_whitespace() {
            if !seen_non_ws {
                continue;
            }
            if !in_ws {
                chars.push(' ');
                spans.push((idx, idx + ch.len_utf8()));
                in_ws = true;
            } else if let Some(last) = spans.last_mut() {
                last.1 = idx + ch.len_utf8();
            }
        } else {
            seen_non_ws = true;
            in_ws = false;
            chars.push(ch);
            spans.push((idx, idx + ch.len_utf8()));
        }
    }

    while matches!(chars.last(), Some(' ')) {
        chars.pop();
        spans.pop();
    }

    (chars.into_iter().collect(), spans)
}

/// Find `needle` in `haystack` using whitespace-normalised matching.
///
/// Uses pre-computed cumulative byte offsets to achieve O(N) complexity instead
/// of the previous O(N²) approach that recalculated byte offsets in nested loops.
pub(crate) fn find_normalised_whitespace(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let norm_needle = normalise_ws(needle);
    if norm_needle.is_empty() {
        return None;
    }
    let needle_char_count = norm_needle.chars().count();
    // Use split_inclusive('\n') so line strings retain their terminators.
    // This handles both \n (Unix) and \r\n (Windows) correctly because the
    // terminator bytes are included in the string, and cumulative offsets
    // are derived from actual string lengths rather than a fixed +1 guess.
    let lines: Vec<&str> = haystack.split_inclusive('\n').collect();

    // Pre-compute cumulative byte offsets for O(1) line-to-byte lookup.
    // line_offsets[i] = byte offset of the start of line i.
    // Line length already includes the terminator (\n or \r\n).
    let mut line_offsets: Vec<usize> = Vec::with_capacity(lines.len());
    let mut cumulative: usize = 0;
    for line in &lines {
        line_offsets.push(cumulative);
        cumulative += line.len();
    }

    let max_window = needle.lines().count() + 5;
    for start_line in 0..lines.len() {
        let window_end = lines.len().min(start_line + max_window);
        let byte_start = line_offsets[start_line];
        for end_line in start_line..window_end {
            let byte_end = line_offsets[end_line] + lines[end_line].len();
            let window = &haystack[byte_start..byte_end];
            let (norm_window, spans) = normalise_ws_with_spans(window);
            if let Some(match_byte_start) = norm_window.find(&norm_needle) {
                let match_char_start = norm_window[..match_byte_start].chars().count();
                let match_char_end = match_char_start + needle_char_count;
                let &(span_start, _) = spans.get(match_char_start)?;
                let &(_, span_end) = spans.get(match_char_end.saturating_sub(1))?;
                return Some((byte_start + span_start, span_end - span_start));
            }
        }
    }
    None
}

/// Generate a structured diff between two strings. Handlers return the
/// JSON shape (so the LLM gets a clean, parseable diff) and callers
/// that want a string render it via `output::render_unified_diff` or
/// `output::render_split_diff`.
pub(crate) fn make_diff(
    original: &str,
    modified: &str,
    file_path: &str,
) -> crate::cli::mcp::output::DiffResult {
    crate::cli::mcp::output::compute_diff(original, modified, file_path)
}

/// JSON schema for the phase analysis tool arguments.
pub(crate) fn phase_analysis_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "phase": {
                "oneOf": [
                    { "type": "integer", "minimum": 1, "maximum": 5 },
                    { "type": "string", "enum": ["all", "1", "2", "3", "4", "5"] }
                ],
                "default": "all"
            },
            "project_path": {
                "type": "string",
                "description": "Project directory (auto-indexes on first use; omit to use current project)"
            },
            "mode": {
                "type": "string",
                "enum": ["ultra", "balanced", "verbose"],
                "default": "balanced"
            },
            "path": {
                "type": "string",
                "description": "File or directory to analyze (defaults to project root)"
            },
            "max_files": {
                "type": "integer",
                "default": 2000
            },
            "max_focus_files": {
                "type": "integer",
                "default": 20
            },
            "top_n": {
                "type": "integer",
                "default": 10
            },
            "max_chars": {
                "type": "integer",
                "default": 12000
            },
            "include_docs": {
                "type": "boolean",
                "description": "IMPORTANT: Enable to include prose/documentation files (README, docs/, *.md) \
    in the analysis. Without this, only source code files are analyzed. Set to true when you need \
    architectural docs, changelogs, or project documentation. Also accepts strings: 'true'/'false'.",
                "default": false
            },
            "docs_mode": {
                "type": "string",
                "enum": ["off", "markdown", "text", "all"],
                "description": "Controls which documentation files to include: 'off' (default, code only), \
    'markdown' (*.md files like README, CHANGELOG), 'text' (*.txt, *.rst), 'all' (all doc formats). \
    Use 'markdown' or 'all' to analyze project documentation alongside code.",
                "default": "off"
            }
        },
        "required": []
    })
}

/// Shared test helper: creates a `ProjectRegistry` with a single project rooted at `path`.
#[cfg(test)]
pub(crate) fn test_registry_for(
    path: &std::path::Path,
) -> std::sync::Arc<crate::cli::registry::ProjectRegistry> {
    let leindex = crate::cli::leindex::LeIndex::new(path).expect("leindex");
    std::sync::Arc::new(crate::cli::registry::ProjectRegistry::with_initial_project(
        5, leindex,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        assert!(extract_bool(&args, "flag", false));
        assert!(!extract_bool(&args, "off", true));
    }

    #[test]
    fn test_extract_bool_string_coercion() {
        let args = serde_json::json!({
            "a": "true", "b": "false", "c": "1", "d": "0", "e": "yes", "f": "no",
            "g": "TRUE", "h": "False"
        });
        assert!(extract_bool(&args, "a", false));
        assert!(!extract_bool(&args, "b", true));
        assert!(extract_bool(&args, "c", false));
        assert!(!extract_bool(&args, "d", true));
        assert!(extract_bool(&args, "e", false));
        assert!(!extract_bool(&args, "f", true));
        assert!(extract_bool(&args, "g", false));
        assert!(!extract_bool(&args, "h", true));
    }

    #[test]
    fn test_extract_bool_number_coercion() {
        let args = serde_json::json!({"one": 1, "zero": 0, "big": 42});
        assert!(extract_bool(&args, "one", false));
        assert!(!extract_bool(&args, "zero", true));
        assert!(extract_bool(&args, "big", false));
    }

    #[test]
    fn test_extract_bool_missing_uses_default() {
        let args = serde_json::json!({});
        assert!(extract_bool(&args, "absent", true));
        assert!(!extract_bool(&args, "absent", false));
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
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Function),
            "function"
        );
        assert_eq!(node_type_str(&crate::graph::pdg::NodeType::Class), "class");
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Method),
            "method"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Variable),
            "variable"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::Module),
            "module"
        );
        assert_eq!(
            node_type_str(&crate::graph::pdg::NodeType::External),
            "external"
        );
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
    fn test_read_source_snippet_rejects_out_of_bounds_range() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, b"0123456789").unwrap();
        let path = file.to_str().unwrap();
        assert!(read_source_snippet(path, (0, 11)).is_none());
        assert!(read_source_snippet(path, (11, 12)).is_none());
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
        let cid = pdg.add_node(crate::graph::pdg::Node {
            id: "caller".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "caller".into(),
            file_path: "a.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
        });
        let did = pdg.add_node(crate::graph::pdg::Node {
            id: "callee".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "callee".into(),
            file_path: "b.rs".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".into(),
        });
        pdg.add_call_graph_edges(vec![(cid, did)]);
        let callers = get_direct_callers(&pdg, did);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0], cid);
    }

    #[test]
    fn test_replace_whole_word_basic() {
        assert_eq!(
            replace_whole_word("foo bar baz", "bar", "qux"),
            "foo qux baz"
        );
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
        if let EditChange::ReplaceText {
            start,
            end,
            new_text,
        } = &changes[0]
        {
            assert_eq!(*start, content.find("println!(\"Hello\")").unwrap());
            assert_eq!(*end, *start + "println!(\"Hello\")".len());
            assert_eq!(new_text, "println!(\"Goodbye\")");
        } else {
            panic!("Expected ReplaceText");
        }
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

    #[test]
    fn test_read_source_snippet_partial_range() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.rs");
        std::fs::write(&file, b"0123456789").unwrap();
        let path = file.to_str().unwrap();
        let snippet = read_source_snippet(path, (2, 5));
        assert_eq!(snippet.unwrap(), "234");
    }

    #[test]
    fn test_byte_range_to_line_range_rejects_out_of_bounds() {
        let content = "first line\nsecond line\n";
        assert_eq!(byte_range_to_line_range(content, (0, 32)), (0, 0));
        assert_eq!(byte_range_to_line_range(content, (32, 33)), (0, 0));
    }

    #[test]
    fn test_read_source_snippet_nonexistent_file() {
        assert!(read_source_snippet("/definitely/does/not/exist.rs", (0, 10)).is_none());
    }

    #[test]
    fn test_find_normalised_whitespace_returns_tight_span() {
        let content = "before\nfoo\t  bar\nafter";
        let (start, len) = find_normalised_whitespace(content, "foo bar").unwrap();
        assert_eq!(&content[start..start + len], "foo\t  bar");
        assert_ne!(&content[start..start + len], content);
    }

    #[test]
    fn test_parse_edit_changes_explicit_byte_offsets() {
        // When start_byte and end_byte are provided, use them directly
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "start_byte": 10,
            "end_byte": 20,
            "new_text": "replacement"
        }]);
        let changes = parse_edit_changes(&changes_json, Some("any content")).unwrap();
        assert_eq!(changes.len(), 1);
        if let EditChange::ReplaceText { start, end, .. } = &changes[0] {
            assert_eq!(*start, 10);
            assert_eq!(*end, 20);
        } else {
            panic!("Expected ReplaceText");
        }
    }

    #[test]
    fn test_parse_edit_changes_text_not_found_returns_error() {
        let changes_json = serde_json::json!([{
            "type": "replace_text",
            "old_text": "nonexistent text",
            "new_text": "replacement"
        }]);
        let result = parse_edit_changes(&changes_json, Some("actual file content"));
        assert!(
            result.is_err(),
            "Should error when old_text not found in content"
        );
    }

    #[test]
    fn test_pdg_find_by_name() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(crate::graph::pdg::Node {
            id: "file.py:MyClass.health_check".into(),
            node_type: crate::graph::pdg::NodeType::Method,
            name: "health_check".into(),
            file_path: "file.py".into(),
            byte_range: (0, 50),
            complexity: 2,
            language: "python".into(),
        });

        // find_by_symbol with full ID works
        assert_eq!(pdg.find_by_symbol("file.py:MyClass.health_check"), Some(n1));

        // find_by_name with short name works
        assert_eq!(pdg.find_by_name("health_check"), Some(n1));

        // find_by_symbol with short name does NOT work (that's the old bug)
        assert_eq!(pdg.find_by_symbol("health_check"), None);
    }

    #[test]
    fn test_pdg_find_by_name_in_file() {
        let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
        let n1 = pdg.add_node(crate::graph::pdg::Node {
            id: "a.py:run".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "a.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
        });
        let n2 = pdg.add_node(crate::graph::pdg::Node {
            id: "b.py:run".into(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "run".into(),
            file_path: "b.py".into(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".into(),
        });

        // Without file hint, returns first match
        assert!(pdg.find_by_name("run").is_some());

        // With file hint, returns correct one
        assert_eq!(pdg.find_by_name_in_file("run", Some("a.py")), Some(n1));
        assert_eq!(pdg.find_by_name_in_file("run", Some("b.py")), Some(n2));
    }

    #[test]
    fn test_make_diff_generates_structured_diff() {
        let original = "line1\nline2\nline3\n";
        let modified = "line1\nmodified\nline3\n";
        let diff = make_diff(original, modified, "test.rs");
        assert_eq!(diff.file_path, "test.rs");
        assert_eq!(diff.additions, 1);
        assert_eq!(diff.deletions, 1);
        assert!(diff.has_changes());
    }
}
