use super::helpers::{
    extract_bool, extract_string, extract_usize, glob_match, node_type_str, resolve_scope,
    wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use regex::RegexBuilder;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_text_search — raw text/regex search across files.
#[derive(Clone)]
pub struct TextSearchHandler;

fn strip_line_ending(line: &str) -> &str {
    line.trim_end_matches(['\r', '\n'])
}

#[allow(missing_docs)]
impl TextSearchHandler {
    pub fn name(&self) -> &str {
        "leindex_text_search"
    }

    pub fn description(&self) -> &str {
        "PRIMARY text search — use instead of Grep/rg. Returns exact matching lines with \
file:line and the owning symbol name+type for each match. One call replaces Grep + Read \
to understand match context. Supports regex, globs, scope, and context_lines."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Text pattern to search for (literal or regex)"
                },
                "is_regex": {
                    "type": "boolean",
                    "description": "Treat query as regex (default: false = literal match)",
                    "default": false
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default: false)",
                    "default": false
                },
                "include_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only search files matching these globs, e.g. [\"*.rs\", \"*.ts\"]"
                },
                "exclude_globs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude files matching these globs, e.g. [\"*_test.rs\"]"
                },
                "scope": {
                    "type": "string",
                    "description": "Restrict search to a directory path"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum results to return (default: 100)",
                    "default": 100,
                    "minimum": 1,
                    "maximum": 1000
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip the first N results for pagination (default: 0)",
                    "default": 0,
                    "minimum": 0
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context above/below each match (default: 2)",
                    "default": 2,
                    "minimum": 0,
                    "maximum": 10
                }
            },
            "required": ["query"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let query = extract_string(&args, "query")?;
        let is_regex = extract_bool(&args, "is_regex", false);
        let case_sensitive = extract_bool(&args, "case_sensitive", false);
        let max_results = extract_usize(&args, "max_results", 100)?.min(1000);
        let offset = extract_usize(&args, "offset", 0)?;
        let context_lines = extract_usize(&args, "context_lines", 2)?.min(10);

        let include_globs: Vec<String> = args
            .get("include_globs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let exclude_globs: Vec<String> = args
            .get("exclude_globs")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let project_path = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        guard
            .ensure_pdg_loaded()
            .map_err(|e| JsonRpcError::indexing_failed(format!("Failed to load PDG: {}", e)))?;

        let scope = resolve_scope(&args, guard.project_path())?;

        // Build regex or literal matcher
        let regex = if is_regex {
            let re = RegexBuilder::new(&query)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|e| {
                    JsonRpcError::invalid_params(format!("Invalid regex '{}': {}", query, e))
                })?;
            Some(re)
        } else {
            None
        };

        let search_query = if case_sensitive {
            query.clone()
        } else {
            query.to_lowercase()
        };

        // Get PDG for enrichment (optional — works without it)
        let pdg = guard.pdg();

        // Collect source files from the project
        let project_root = guard.project_path();
        let mut results: Vec<Value> = Vec::new();

        // Dirs to always skip
        use crate::cli::skip_dirs::SKIP_DIRS;

        for entry in walkdir::WalkDir::new(project_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.iter().any(|s| name == *s)
            })
        {
            if results.len() >= max_results {
                break;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_file() {
                continue;
            }

        let file_path = entry.path();
        let file_path_str = file_path.to_string_lossy();

            // Apply scope filter
            if let Some(ref s) = scope {
                if !file_path_str.starts_with(s.as_str()) {
                    continue;
                }
            }

            // Apply include globs
            if !include_globs.is_empty() {
                let matches_any = include_globs.iter().any(|g| glob_match(&file_path_str, g));
                if !matches_any {
                    continue;
                }
            }

            // Apply exclude globs
            if exclude_globs.iter().any(|g| glob_match(&file_path_str, g)) {
                continue;
            }

            // Read file content
            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue, // Skip binary or unreadable files
            };

            // Use split_inclusive to preserve line endings for accurate byte offset calculation
            // This handles both Unix (\n) and Windows (\r\n) line endings correctly
            let lines: Vec<&str> = content.split_inclusive('\n').collect();

            // Precompute cumulative byte offsets (prefix sums) for O(1) lookup per match.
            // Without this, the per-match sum `lines[..line_idx].iter().map(|l| l.len()).sum()`
            // would be O(N²) in total across all matches.
            let line_byte_offsets: Vec<usize> = lines
                .iter()
                .scan(0usize, |acc, l| {
                    let offset = *acc;
                    *acc += l.len();
                    Some(offset)
                })
                .collect();

            for (line_idx, line) in lines.iter().enumerate() {
                if results.len() >= max_results {
                    break;
                }

                // Strip line ending for matching (handles both \n and \r\n)
                let line_without_ending = strip_line_ending(line);

                let matched = if let Some(ref re) = regex {
                    re.is_match(line_without_ending)
                } else if case_sensitive {
                    line_without_ending.contains(&search_query)
                } else {
                    line_without_ending.to_lowercase().contains(&search_query)
                };

                if !matched {
                    continue;
                }

                let line_number = line_idx + 1; // 1-indexed

                // Collect context lines - strip line endings for display
                let ctx_before: Vec<String> = (line_idx.saturating_sub(context_lines)..line_idx)
                    .map(|i| format!("{}: {}", i + 1, strip_line_ending(lines[i])))
                    .collect();
                let ctx_after: Vec<String> = ((line_idx + 1)
                    ..((line_idx + 1 + context_lines).min(lines.len())))
                    .map(|i| format!("{}: {}", i + 1, strip_line_ending(lines[i])))
                    .collect();

                // Compact PDG enrichment: just symbol name + type (~4 tokens)
                // Eliminates follow-up Read to understand what code this match is in
                let (in_symbol, symbol_type) = pdg
                    .and_then(|pdg| {
                        // O(1) lookup from precomputed prefix sums — avoids O(N²) recomputation
                        let byte_offset: usize = line_byte_offsets[line_idx];

                        let nodes = pdg.nodes_in_file(&file_path_str);
                        let mut best: Option<(crate::graph::pdg::NodeId, usize)> = None;

                        for nid in nodes {
                            if let Some(node) = pdg.get_node(nid) {
                                let (start, end) = node.byte_range;
                                if byte_offset >= start && byte_offset < end {
                                    let range_size = end - start;
                                    if best.map_or(true, |(_, sz)| range_size < sz) {
                                        best = Some((nid, range_size));
                                    }
                                }
                            }
                        }

                        best.and_then(|(nid, _)| {
                            pdg.get_node(nid).map(|node| {
                                (node.name.clone(), node_type_str(&node.node_type).to_owned())
                            })
                        })
                    })
                    .map(|(name, typ)| (Some(name), Some(typ)))
                    .unwrap_or((None, None));

                let mut entry = serde_json::json!({
                    "file": file_path_str,
                    "line": line_number,
                    "content": line_without_ending,
                });

                // Only include context lines when requested (context_lines > 0)
                if !ctx_before.is_empty() {
                    entry["before"] = serde_json::json!(ctx_before);
                }
                if !ctx_after.is_empty() {
                    entry["after"] = serde_json::json!(ctx_after);
                }

                // Compact symbol annotation — always present when PDG available
                if let Some(sym) = in_symbol {
                    entry["in_symbol"] = Value::String(sym);
                }
                if let Some(typ) = symbol_type {
                    entry["symbol_type"] = Value::String(typ);
                }

                results.push(entry);
            }
        }

        let total = results.len();
        let paginated: Vec<Value> = results.into_iter().skip(offset).collect();
        let count = paginated.len();

        Ok(wrap_with_meta(
            serde_json::json!({
                "query": query,
                "is_regex": is_regex,
                "offset": offset,
                "count": count,
                "total_matched": total,
                "has_more": offset + count < total,
                "results": paginated,
            }),
            &guard,
        ))
    }
}
