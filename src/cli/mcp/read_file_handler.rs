use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
};
use super::protocol::JsonRpcError;
use super::read_paths::{self, ConsultVerdict};
use crate::cli::live_project::LiveProject;
use crate::cli::registry::{ProjectHandle, ProjectRegistry};
use crate::graph::pdg::ProgramDependenceGraph;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Handler for LeIndex [read_file — PDG-annotated file read.
#[derive(Clone)]
pub struct ReadFileHandler;

fn line_byte_offsets(content: &str) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(content.lines().count() + 1);
    offsets.push(0);
    let mut acc: usize = 0;
    for chunk in content.split_inclusive('\n') {
        acc += chunk.len();
        offsets.push(acc);
    }
    offsets
}

fn byte_to_line_start(offsets: &[usize], byte_pos: usize, total_lines: usize) -> usize {
    offsets
        .partition_point(|&off| off <= byte_pos)
        .min(total_lines.max(1))
}

fn byte_to_line_end(offsets: &[usize], byte_pos: usize, total_lines: usize) -> usize {
    offsets
        .partition_point(|&off| off < byte_pos)
        .min(total_lines.max(1))
}

const LANGUAGE_BY_EXTENSION: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("js", "javascript"),
    ("mjs", "javascript"),
    ("cjs", "javascript"),
    ("ts", "typescript"),
    ("mts", "typescript"),
    ("cts", "typescript"),
    ("tsx", "typescriptreact"),
    ("jsx", "javascriptreact"),
    ("go", "go"),
    ("java", "java"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("hpp", "cpp"),
    ("cc", "cpp"),
    ("rb", "ruby"),
    ("php", "php"),
    ("swift", "swift"),
    ("kt", "kotlin"),
    ("cs", "csharp"),
    ("lua", "lua"),
    ("zig", "zig"),
    ("md", "markdown"),
    ("json", "json"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("html", "html"),
    ("css", "css"),
    ("scss", "scss"),
    ("sql", "sql"),
    ("sh", "shell"),
    ("bash", "shell"),
];

fn detect_language(extension: &str) -> &str {
    LANGUAGE_BY_EXTENSION
        .iter()
        .find(|(known_extension, _)| *known_extension == extension)
        .map_or(extension, |(_, language)| *language)
}

/// Project root for handle parsing and PDG enrichment labels.
///
/// An unindexed or unresolved project is NOT an error for a read: an
/// absolute path outside any project is a valid live read (brief §6.1), and
/// a relative path only needs the root when it is actually relative.
async fn resolve_project_root(registry: &Arc<ProjectRegistry>, args: &Value) -> Option<PathBuf> {
    if let Some(project_path) = args.get("project_path").and_then(|value| value.as_str()) {
        return LiveProject::resolve(project_path)
            .ok()
            .map(|project| project.root().to_path_buf());
    }

    registry.default_project_path().await.ok()
}

/// Resolve the file argument to a path on disk. Absolute paths read from
/// disk as-is — the project boundary is irrelevant to a live read (§6.1),
/// which is what makes logs, /tmp files and other repos readable. Relative
/// paths join against the project root.
fn resolve_read_target(
    file_path: &str,
    project_root: Option<&Path>,
) -> Result<PathBuf, JsonRpcError> {
    let candidate = Path::new(file_path);
    if candidate.is_absolute() {
        return Ok(candidate.to_path_buf());
    }
    let Some(root) = project_root else {
        return Err(JsonRpcError::invalid_params(format!(
            "Relative path '{file_path}' needs a project: pass project_path, or use an absolute file_path"
        )));
    };
    Ok(root.join(file_path))
}

/// Live bytes of `path`: full content plus the line count. A read never
/// touches the index — the bytes on disk are the answer (brief §3.10).
async fn read_file_content(resolved_file_path: &Path) -> Result<(String, usize), JsonRpcError> {
    let content = tokio::fs::read_to_string(resolved_file_path)
        .await
        .map_err(|error| {
            JsonRpcError::invalid_params(format!(
                "Cannot read file '{}': {}",
                resolved_file_path.display(),
                error
            ))
        })?;
    let total_lines = content.lines().count();
    Ok((content, total_lines))
}

/// Validate the requested window against the file and slice it (1-based,
/// inclusive bounds).
fn visible_slice(
    content: &str,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
) -> Result<String, JsonRpcError> {
    if total_lines > 0 && start_line > total_lines {
        return Err(JsonRpcError::invalid_params(format!(
            "start_line {start_line} exceeds total lines {total_lines}"
        )));
    }
    if total_lines > 0 && end_line < start_line {
        return Err(JsonRpcError::invalid_params(format!(
            "end_line {end_line} precedes start_line {start_line}"
        )));
    }
    Ok(content
        .lines()
        .skip(start_line - 1)
        .take(end_line.saturating_sub(start_line - 1))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn visible_byte_range(
    line_offsets: &[usize],
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    content_len: usize,
) -> (usize, usize) {
    (
        line_offsets.get(start_line - 1).copied().unwrap_or(0),
        line_offsets
            .get(end_line.min(total_lines))
            .copied()
            .unwrap_or(content_len),
    )
}

fn build_symbol_map(
    pdg: &ProgramDependenceGraph,
    file_path: &str,
    line_offsets: &[usize],
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    content_len: usize,
    include_symbol_map: bool,
) -> Vec<Value> {
    if !include_symbol_map {
        return Vec::new();
    }

    let (visible_start_byte, visible_end_byte) =
        visible_byte_range(line_offsets, start_line, end_line, total_lines, content_len);
    let mut symbols = Vec::new();

    for nid in pdg.nodes_in_file(file_path) {
        let Some(node) = pdg.get_node(nid) else {
            continue;
        };
        let (sym_start, sym_end) = node.byte_range;
        if sym_end <= visible_start_byte || sym_start >= visible_end_byte {
            continue;
        }

        let line_start = byte_to_line_start(line_offsets, sym_start, total_lines);
        let line_end = byte_to_line_end(line_offsets, sym_end, total_lines);
        let caller_count = get_direct_callers(pdg, nid).len();
        let dep_count = pdg.neighbors(nid).len();
        let callers = get_direct_callers(pdg, nid)
            .iter()
            .filter_map(|&cid| pdg.get_node(cid).map(|n| n.name.clone()))
            .take(5)
            .collect::<Vec<_>>();
        let callees = pdg
            .neighbors(nid)
            .iter()
            .filter_map(|&did| pdg.get_node(did).map(|n| n.name.clone()))
            .take(5)
            .collect::<Vec<_>>();

        symbols.push(serde_json::json!({
            "name": node.name,
            "type": node_type_str(&node.node_type),
            "line_start": line_start,
            "line_end": line_end,
            "complexity": node.complexity,
            "caller_count": caller_count,
            "dependency_count": dep_count,
            "callers": callers,
            "callees": callees,
        }));
    }

    symbols.sort_by_key(|symbol| {
        symbol
            .get("line_start")
            .and_then(|value| value.as_u64())
            .unwrap_or(0)
    });
    symbols
}

fn build_pdg_enrichment(
    pdg: &ProgramDependenceGraph,
    file_path: &str,
    content: &str,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    include_symbol_map: bool,
) -> (Vec<Value>, Option<Value>) {
    let file_line_offsets = line_byte_offsets(content);
    let nodes = pdg.nodes_in_file(file_path);
    let mut dep_line_offsets_cache: HashMap<Arc<str>, Vec<usize>> = HashMap::new();

    let symbol_map = build_symbol_map(
        pdg,
        file_path,
        &file_line_offsets,
        start_line,
        end_line,
        total_lines,
        content.len(),
        include_symbol_map,
    );

    let context = {
        let (visible_start_byte, visible_end_byte) = visible_byte_range(
            &file_line_offsets,
            start_line,
            end_line,
            total_lines,
            content.len(),
        );

        let mut symbols_here: Vec<String> = Vec::new();
        let mut imports_from: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut used_by: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        for &nid in &nodes {
            let Some(node) = pdg.get_node(nid) else {
                continue;
            };
            let (sym_start, sym_end) = node.byte_range;

            if sym_end > visible_start_byte && sym_start < visible_end_byte {
                let ls = byte_to_line_start(&file_line_offsets, sym_start, total_lines);
                let le = byte_to_line_end(&file_line_offsets, sym_end, total_lines);
                symbols_here.push(format!("{}(L{}-L{})", node.name, ls, le));
            }

            for &did in &pdg.neighbors(nid) {
                if let Some(dep) = pdg.get_node(did)
                    && dep.file_path != node.file_path
                {
                    let dep_offsets = dep_line_offsets_cache
                        .entry(dep.file_path.clone())
                        .or_insert_with(|| {
                            std::fs::read_to_string(&*dep.file_path)
                                .map(|c| line_byte_offsets(&c))
                                .unwrap_or_default()
                        });
                    if dep_offsets.is_empty() {
                        continue;
                    }
                    let dep_total_lines = dep_offsets.len().saturating_sub(1);
                    let dep_line =
                        byte_to_line_start(dep_offsets, dep.byte_range.0, dep_total_lines);
                    imports_from.insert(format!("{}:{} (L{})", dep.file_path, dep.name, dep_line));
                }
            }

            for &cid in &get_direct_callers(pdg, nid) {
                if let Some(caller) = pdg.get_node(cid)
                    && caller.file_path != node.file_path
                {
                    used_by.insert(format!("{}:{}", caller.file_path, caller.name));
                }
            }
        }

        let imports_vec: Vec<String> = imports_from.into_iter().take(10).collect();
        let used_by_vec: Vec<String> = used_by.into_iter().take(10).collect();

        Some(serde_json::json!({
            "symbols_on_visible_lines": symbols_here,
            "imports_from": imports_vec,
            "used_by": used_by_vec
        }))
    };

    (symbol_map, context)
}

/// Language of a path, from its extension (case-insensitive), or `text`.
/// Unknown extensions report the extension itself, matching
/// [`detect_language`].
fn language_for(file_path: &str) -> String {
    Path::new(file_path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let lowered = extension.to_ascii_lowercase();
            detect_language(&lowered).to_string()
        })
        .unwrap_or_else(|| "text".to_string())
}

/// An explicit symbol-map request must not silently degrade: a handle can lack
/// a resident PDG (fresh one-shot process, or an auto-index swapped in a new
/// LeIndex whose graph was dropped at completion). Load it on demand in that
/// case (audit #7: the flag was a no-op) — but WITHOUT holding the project
/// write lock while doing it. §3.10: a read must never queue behind a
/// background reindex that holds the write lock; the hydration itself is
/// still serialized behind that lock, but this call neither waits for it nor
/// takes the lock at all.
async fn ensure_symbol_map_pdg(
    handle: Option<ProjectHandle>,
    wanted: bool,
) -> Option<ProjectHandle> {
    match handle {
        Some(handle) if wanted => {
            if let Ok(mut guard) = handle.try_write()
                && guard.pdg().is_none()
            {
                // The symbol map lists graph nodes only; the full engine
                // load here added ~1s to every cold read with a map.
                let _ = guard.ensure_pdg_loaded_graph_only();
            }
            // Lock busy (a reindex is running): serve without the map rather
            // than block — the read itself is exact either way.
            Some(handle)
        }
        other => other,
    }
}

/// The resident PDG, cloned for the blocking enrichment task.
///
/// §3.10: use `try_read` so a read never queues behind a background reindex
/// holding the project write lock. A busy lock just means no PDG annotation
/// for this response; the exact bytes are unaffected.
async fn pdg_snapshot(handle: Option<&ProjectHandle>) -> Option<ProgramDependenceGraph> {
    match handle {
        Some(handle) => {
            let guard = handle.try_read().ok()?;
            guard.pdg().cloned()
        }
        None => None,
    }
}

/// Build the PDG enrichment (symbol map plus compact context) off the runtime.
///
/// Takes the resident PDG snapshot exactly once and reports whether it was
/// available, so the caller can label the retrieval status without re-cloning.
async fn pdg_enrichment(
    handle: Option<&ProjectHandle>,
    file_path: &str,
    resolved_file_path: &Path,
    content: &str,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    include_symbol_map: bool,
) -> Result<(Vec<Value>, Option<Value>, &'static str), JsonRpcError> {
    let Some(pdg) = pdg_snapshot(handle).await else {
        return Ok((Vec::new(), None, "not_loaded"));
    };
    let enrichment_file_path = resolved_file_path.to_string_lossy().to_string();
    let content = content.to_string();
    let file_path_label = file_path.to_string();
    let enrichment = tokio::task::spawn_blocking(move || {
        build_pdg_enrichment(
            &pdg,
            &enrichment_file_path,
            &content,
            start_line,
            end_line,
            total_lines,
            include_symbol_map,
        )
    })
    .await
    .map_err(|e| {
        JsonRpcError::internal_error(format!(
            "Failed to build PDG enrichment for '{}': {}",
            file_path_label, e
        ))
    })?;
    let (symbol_map, context) = enrichment;
    Ok((symbol_map, context, "fresh"))
}

/// Attach the read-path freshness badge WITHOUT touching the project lock
/// (§3.10). Reads the health snapshot file directly, so a background
/// reindex holding the write lock cannot stall or gate a live read.
fn wrap_read_result(result: Value, project_root: &Path) -> Value {
    super::helpers::wrap_read_live_meta(result, project_root)
}

#[allow(missing_docs)]
impl ReadFileHandler {
    pub fn name(&self) -> &str {
        "leindex_read_file"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Read File]"
    }

    pub fn description(&self) -> &str {
        "PRIMARY file reader — live exact bytes with single line numbers, stable handles \
(path#symbol, path#L600-800), optional elided views (focus_symbol) and session dedup \
(re-reads of unchanged content answer `unchanged since read #N`; fresh:true bypasses). \
Works for any absolute path, including outside the project — no index required."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "File to read: absolute path (anywhere on disk), project-relative path, or handle (path#symbol, path#L600-800)"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Start line, 1-indexed (default: 1)",
                    "default": 1,
                    "minimum": 1
                },
                "end_line": {
                    "type": "integer",
                    "description": "End line, 1-indexed inclusive (default: end of file)",
                    "minimum": 1
                },
                "max_lines": {
                    "type": "integer",
                    "description": "Maximum lines to return (default: 500, safety cap)",
                    "default": 500,
                    "minimum": 1,
                    "maximum": 2000
                },
                "focus_symbol": {
                    "type": "string",
                    "description": "Elided read: this symbol in full, sibling top-level items collapsed to one-line signatures with [expand: path#item] handles"
                },
                "fresh": {
                    "type": "boolean",
                    "description": "Bypass session dedup and serve the bytes even if unchanged since a previous read",
                    "default": false
                },
                "include_symbol_map": {
                    "type": "boolean",
                    "description": "Include PDG symbol annotations (default: false). \
        Set true when structural context is useful.",
                    "default": false
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory for relative paths and PDG annotations. Reads never require an index."
                }
            },
            "required": ["file_path"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_arg = extract_string(&args, "file_path")?;
        let max_lines = extract_usize(&args, "max_lines", 500)?.min(2000);
        let include_symbol_map = extract_bool(&args, "include_symbol_map", false);
        let fresh = extract_bool(&args, "fresh", false);
        let focus_symbol = args
            .get("focus_symbol")
            .and_then(Value::as_str)
            .map(str::to_string);

        // An unindexed or unresolved project is fine: absolute paths read
        // live regardless (§6.1); only relative paths need the root.
        let project_root = resolve_project_root(registry, &args).await;

        // Handle form: `path`, `path#symbol`, `path#L600-800` (§4.1).
        let handle = read_paths::parse_read_handle(&file_arg, project_root.as_deref());
        let resolved_file_path = resolve_read_target(&handle.file_path, project_root.as_deref())?;

        // THE read: live bytes, no index, no project lock (§3.10).
        let (content, total_lines) = read_file_content(&resolved_file_path).await?;
        let language = language_for(&handle.file_path);

        // Window resolution. Precedence: explicit start/end args, then the
        // #L600-800 fragment, then a symbol anchor, then the whole file.
        let explicit_start = extract_usize(&args, "start_line", 0)?;
        let explicit_end = extract_usize(&args, "end_line", 0)?;
        let symbol_target = handle.symbol.clone().or_else(|| focus_symbol.clone());

        let mut anchor: Option<(usize, usize)> = None;
        if let Some(target) = &symbol_target {
            let live =
                super::read_symbol_handler::parse_live_file(resolved_file_path.clone()).await?;
            let spans = read_paths::spans_with_lines(&content, &live.symbols);
            let span = read_paths::find_span(&spans, target).ok_or_else(|| {
                JsonRpcError::invalid_params(format!(
                    "Symbol '{target}' not found in {}",
                    resolved_file_path.display()
                ))
            })?;
            anchor = Some((span.start_line, span.end_line));
        }
        // No anchor and no explicit range means the WHOLE file — defaulting
        // the end to 1 collapsed every plain read to its first line.
        let (anchor_start, anchor_end) = anchor.unwrap_or((1, total_lines.max(1)));
        let start_line = if explicit_start > 0 {
            explicit_start.max(1)
        } else {
            handle
                .line_range
                .map(|(start, _)| start)
                .unwrap_or(anchor_start)
        };
        let end_line = if explicit_end > 0 {
            explicit_end
        } else {
            handle.line_range.map(|(_, end)| end).unwrap_or(anchor_end)
        }
        .min(total_lines.max(1))
        .min(start_line.saturating_add(max_lines).saturating_sub(1));

        let session = args
            .get("_session")
            .and_then(Value::as_str)
            .unwrap_or("__process__");
        let ledger = read_paths::session_ledger();
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        let range_handle = read_paths::handle_for_range(
            &resolved_file_path,
            project_root.as_deref(),
            start_line,
            end_line,
        );

        // Elided view (§4.1): focus symbol in full, siblings collapsed.
        // Always served — a view is not the same answer as the bytes.
        if let Some(focus) = &focus_symbol {
            let live =
                super::read_symbol_handler::parse_live_file(resolved_file_path.clone()).await?;
            let file_label = project_root
                .as_deref()
                .and_then(|root| resolved_file_path.strip_prefix(root).ok())
                .map(|rel| rel.to_string_lossy().into_owned())
                .unwrap_or_else(|| resolved_file_path.to_string_lossy().into_owned());
            let elided =
                read_paths::elide_around(&content, &live.symbols, focus, &file_label, max_lines)
                    .map_err(JsonRpcError::invalid_params)?;

            let mut result = serde_json::json!({
                "file_path": handle.file_path,
                "language": language,
                "total_lines": total_lines,
                "elided": true,
                "focus_symbol": focus,
                "focus_line_start": elided.focus_start_line,
                "focus_line_end": elided.focus_end_line,
                "expand": elided.expand,
                "handle": range_handle,
                "content": elided.body,
                "collapsed": elided.collapsed.iter().map(|item| serde_json::json!({
                    "name": item.name,
                    "signature": item.signature,
                    "line": item.start_line,
                    "lines": item.line_count,
                    "expand": format!("{file_label}#{}", item.name),
                })).collect::<Vec<_>>(),
                "retrieval": serde_json::json!({
                    "tfidf_status": "not_used_exact",
                    "pdg_status": "not_needed_live",
                    "neural_status": "not_used_exact"
                }),
            });
            result = super::read_paths::attach_range_diagnostics(
                result,
                project_root.as_deref(),
                &resolved_file_path,
                elided.focus_start_line,
                elided.focus_end_line,
            );
            result = wrap_read_result(
                result,
                project_root.as_deref().unwrap_or(&resolved_file_path),
            );
            // The focus bytes were served in full — record that range.
            ledger.record(
                session,
                &resolved_file_path,
                &content_hash,
                elided.focus_start_line,
                elided.focus_end_line,
            );
            return Ok(result);
        }

        // Session dedup (§4.1): an unchanged re-read costs bytes, not tokens.
        if !fresh {
            match ledger.consult(
                session,
                &resolved_file_path,
                &content_hash,
                start_line,
                end_line,
            ) {
                ConsultVerdict::Unchanged {
                    since_ordinal,
                    start_line,
                    end_line,
                } => {
                    let mut payload = read_paths::unchanged_payload(
                        &handle.file_path,
                        since_ordinal,
                        start_line,
                        end_line,
                    );
                    payload["language"] = serde_json::json!(language);
                    payload["total_lines"] = serde_json::json!(total_lines);
                    payload["handle"] = serde_json::json!(range_handle);
                    payload["retrieval"] = serde_json::json!({
                        "tfidf_status": "not_used_exact",
                        "pdg_status": "not_needed_live",
                        "neural_status": "not_used_exact"
                    });
                    let payload = super::read_paths::attach_range_diagnostics(
                        payload,
                        project_root.as_deref(),
                        &resolved_file_path,
                        start_line,
                        end_line,
                    );
                    return Ok(wrap_read_result(
                        payload,
                        project_root.as_deref().unwrap_or(&resolved_file_path),
                    ));
                }
                ConsultVerdict::PartialOverlap {
                    unseen,
                    since_ordinal,
                } => {
                    // Serve only the unseen lines; reference the rest.
                    let mut body = String::new();
                    let mut served_ranges: Vec<(usize, usize)> = Vec::new();
                    for (s, e) in &unseen {
                        let end = (*e).min(total_lines.max(1));
                        if let Ok(slice) = visible_slice(&content, *s, end, total_lines) {
                            let numbered = read_paths::number_lines(&slice, *s);
                            if !body.is_empty() {
                                body.push('\n');
                            }
                            body.push_str(&numbered);
                            served_ranges.push((*s, end));
                        }
                    }
                    let mut result = serde_json::json!({
                        "file_path": handle.file_path,
                        "language": language,
                        "total_lines": total_lines,
                        "start_line": start_line,
                        "end_line": end_line,
                        "unchanged_since_read": since_ordinal,
                        "unchanged_ranges": format!("{start_line}-{end_line} minus served ranges below"),
                        "handle": range_handle,
                        "content": body,
                    });
                    for (s, e) in &served_ranges {
                        ledger.record(session, &resolved_file_path, &content_hash, *s, *e);
                    }
                    result["retrieval"] = serde_json::json!({
                        "tfidf_status": "not_used_exact",
                        "pdg_status": "not_needed_live",
                        "neural_status": "not_used_exact"
                    });
                    let result = super::read_paths::attach_range_diagnostics(
                        result,
                        project_root.as_deref(),
                        &resolved_file_path,
                        start_line,
                        end_line,
                    );
                    return Ok(wrap_read_result(
                        result,
                        project_root.as_deref().unwrap_or(&resolved_file_path),
                    ));
                }
                ConsultVerdict::New => {}
            }
        }

        let content_str = visible_slice(&content, start_line, end_line, total_lines)?;

        let maybe_handle = ensure_symbol_map_pdg(
            registry
                .try_get_loaded(project_root.as_deref().unwrap_or(Path::new("")))
                .await,
            include_symbol_map,
        )
        .await;
        let (symbol_map, context, pdg_status) = pdg_enrichment(
            maybe_handle.as_ref(),
            &handle.file_path,
            &resolved_file_path,
            &content,
            start_line,
            end_line,
            total_lines,
            include_symbol_map,
        )
        .await?;

        let mut result = serde_json::json!({
            "file_path": handle.file_path,
            "language": language,
            "total_lines": total_lines,
            "start_line": start_line,
            "end_line": end_line,
            "handle": range_handle,
            "content": read_paths::number_lines(&content_str, start_line),
        });

        // Symbol-anchored reads report what they resolved to.
        if let Some((s, e)) = anchor {
            result["anchor_symbol"] = serde_json::json!(symbol_target.clone().unwrap_or_default());
            result["anchor_line_start"] = serde_json::json!(s);
            result["anchor_line_end"] = serde_json::json!(e);
        }

        // Always attach compact context when available
        if let Some(ctx) = context {
            result["context"] = ctx;
        }

        result["retrieval"] = serde_json::json!({
            "tfidf_status": "not_used_exact",
            "pdg_status": pdg_status,
            "neural_status": "not_used_exact"
        });

        // Verbose symbol map only when explicitly requested
        if !symbol_map.is_empty() {
            result["symbol_map"] = serde_json::json!(symbol_map);
        }

        result = super::read_paths::attach_range_diagnostics(
            result,
            project_root.as_deref(),
            &resolved_file_path,
            start_line,
            end_line,
        );
        result = wrap_read_result(
            result,
            project_root.as_deref().unwrap_or(&resolved_file_path),
        );

        ledger.record(
            session,
            &resolved_file_path,
            &content_hash,
            start_line,
            end_line,
        );

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    /// Isolated session key: tests must never share ledger state with each
    /// other or with the process-default session.
    fn test_session(name: &str) -> String {
        format!("test-{name}-{}", std::process::id())
    }

    const SAMPLE: &str = "/// Doc.\n#[derive(Debug)]\npub fn alpha() -> u32 {\n    1\n}\n\n/// Beta doc.\npub fn beta(x: u32) -> u32 {\n    x + 1\n}\n\npub struct Gamma {\n    pub field: u32,\n}\n";

    fn write_sample(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, SAMPLE).unwrap();
        path
    }

    #[tokio::test]
    async fn test_read_file_numbers_lines_exactly_once() {
        // §3.1 regression: the numbered body is `N code`, never `N: code` and
        // never a second gutter.
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let result = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("dedup-basic"),
                }),
            )
            .await
            .unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.starts_with("1 /// Doc.\n"), "{content}");
        assert!(content.contains("2 #[derive(Debug)]"), "{content}");
        assert!(content.contains("8 pub fn beta"), "{content}");
        assert!(!content.contains("│"), "no gutter bar: {content}");
        assert!(!content.contains("1:"), "no colon number: {content}");
    }

    #[tokio::test]
    async fn test_second_identical_read_returns_unchanged_marker() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let session = test_session("unchanged");
        let args = serde_json::json!({
            "file_path": path.to_string_lossy(),
            "project_path": dir.path().to_string_lossy(),
            "_session": session,
        });
        let first = ReadFileHandler
            .execute(&registry, args.clone())
            .await
            .unwrap();
        assert!(first.get("status").is_none(), "first read serves bytes");
        let second = ReadFileHandler.execute(&registry, args).await.unwrap();
        assert_eq!(second["status"], "unchanged", "{}", second);
        assert!(
            second["note"]
                .as_str()
                .unwrap()
                .contains("unchanged since read #1 (lines 1-"),
            "{}",
            second["note"].as_str().unwrap()
        );
    }

    #[tokio::test]
    async fn test_fresh_opt_out_bypasses_dedup() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let session = test_session("fresh");
        let base = serde_json::json!({
            "file_path": path.to_string_lossy(),
            "project_path": dir.path().to_string_lossy(),
            "_session": session,
        });
        ReadFileHandler
            .execute(&registry, base.clone())
            .await
            .unwrap();
        let mut fresh_args = base.clone();
        fresh_args["fresh"] = serde_json::json!(true);
        let again = ReadFileHandler
            .execute(&registry, fresh_args)
            .await
            .unwrap();
        assert!(
            again.get("status").is_none() && again["content"].as_str().is_some(),
            "fresh:true serves bytes: {}",
            again
        );
    }

    #[tokio::test]
    async fn test_edit_invalidates_dedup_and_next_read_serves_bytes() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let session = test_session("edit");
        let args = serde_json::json!({
            "file_path": path.to_string_lossy(),
            "project_path": dir.path().to_string_lossy(),
            "_session": session,
        });
        ReadFileHandler
            .execute(&registry, args.clone())
            .await
            .unwrap();
        // External modification of the file changes the content hash — the
        // ledger must serve the new bytes, never a stale `unchanged`.
        std::fs::write(&path, SAMPLE.replace("x + 1", "x + 2")).unwrap();
        let after = ReadFileHandler.execute(&registry, args).await.unwrap();
        assert!(
            after.get("status").is_none() && after["content"].as_str().is_some(),
            "changed file must be re-served: {}",
            after
        );
        assert!(after["content"].as_str().unwrap().contains("x + 2"));
    }

    #[tokio::test]
    async fn test_read_outside_project_reads_live_without_index() {
        // §6.1: logs, /tmp and other repos are readable — no index, no root.
        let outside =
            std::env::temp_dir().join(format!("leindex-lane-e-{}-sample.txt", std::process::id()));
        std::fs::write(&outside, "alpha\nbeta\ngamma\n").unwrap();
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let result = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": outside.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("outside"),
                }),
            )
            .await
            .unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("2 beta"), "{content}");
        std::fs::remove_file(&outside).ok();
    }

    #[tokio::test]
    async fn test_handle_expands_line_range_and_symbol() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let range = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": format!("{}#L1-2", path.to_string_lossy()),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("handle-range"),
                }),
            )
            .await
            .unwrap();
        assert_eq!(range["start_line"], 1);
        assert_eq!(range["end_line"], 2);
        assert_eq!(
            range["content"].as_str().unwrap().lines().count(),
            2,
            "{}",
            range
        );

        let symbol = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": format!("{}#alpha", path.to_string_lossy()),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("handle-symbol"),
                }),
            )
            .await
            .unwrap();
        assert_eq!(symbol["anchor_symbol"], "alpha");
        // Item spans start at the item keyword; the doc comment hangs above.
        assert_eq!(symbol["anchor_line_start"], 3);
        assert_eq!(symbol["anchor_line_end"], 5);
    }

    #[tokio::test]
    async fn test_elided_read_collapses_siblings_and_reports_handles() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let result = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("elide"),
                    "focus_symbol": "beta",
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["elided"], true);
        let content = result["content"].as_str().unwrap();
        // The focus item is verbatim; siblings are one-line collapses with
        // their own expand handles.
        assert!(
            content.contains("pub fn beta(x: u32) -> u32 {"),
            "{content}"
        );
        assert!(
            content.contains("[expand: sample.rs#alpha]"),
            "sibling collapsed: {content}"
        );
        assert!(
            content.contains("[expand: sample.rs#Gamma]"),
            "struct collapsed: {content}"
        );
        assert!(!content.contains("pub field: u32"), "{content}");
        assert_eq!(result["expand"], "sample.rs#beta");
        let collapsed = result["collapsed"].as_array().unwrap();
        assert_eq!(collapsed.len(), 2);
    }

    #[tokio::test]
    async fn test_read_never_reports_initializing_for_an_unindexed_project() {
        // §3.10: a live filesystem read is exact bytes; it must never claim
        // another file's reindex state — not `initializing`, on any path.
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let result = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": test_session("freshness"),
                }),
            )
            .await
            .unwrap();
        let serialized = result.to_string();
        assert!(
            !serialized.contains("initializing"),
            "read must not report reindex state: {serialized}"
        );
        assert_eq!(result["_meta"]["read_source"], "live_bytes");
        // An unindexed project has no health snapshot, so the index state is
        // UNKNOWN. The read must not attach a `fresh` badge the index has not
        // earned: no freshness status at all, rather than a guessed one.
        assert!(
            result["_meta"]["freshness"].get("status").is_none(),
            "unknown index state must not read as fresh: {serialized}"
        );
    }

    #[tokio::test]
    async fn test_read_freshness_badge_reflects_a_known_index_state() {
        use crate::cli::leindex::ComponentStatus;
        use crate::cli::leindex::IndexHealth;
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        for (status, expected) in [
            (ComponentStatus::Fresh, "fresh"),
            (ComponentStatus::Stale, "stale"),
            (ComponentStatus::Partial, "stale"),
            (ComponentStatus::Failed, "stale"),
            (ComponentStatus::Unavailable, "_absent_"),
            (ComponentStatus::NotLoaded, "_absent_"),
        ] {
            crate::cli::index_freshness::save_health(
                &storage,
                &IndexHealth {
                    status,
                    generation: 7,
                    ..Default::default()
                },
            )
            .unwrap();
            let result = ReadFileHandler
                .execute(
                    &registry,
                    serde_json::json!({
                        "file_path": path.to_string_lossy(),
                        "project_path": dir.path().to_string_lossy(),
                        "_session": test_session("freshness-known"),
                    }),
                )
                .await
                .unwrap();
            if expected == "_absent_" {
                assert!(
                    result["_meta"]["freshness"].get("status").is_none(),
                    "{status:?} must not be labelled fresh or stale: {result}"
                );
            } else {
                assert_eq!(
                    result["_meta"]["freshness"]["status"], expected,
                    "{status:?}"
                );
                assert_eq!(result["_meta"]["freshness"]["generation"], 7, "{status:?}");
            }
        }
    }

    #[tokio::test]
    async fn test_partial_overlap_serves_only_unseen_lines() {
        let dir = tempdir().unwrap();
        let path = write_sample(dir.path(), "sample.rs");
        let registry = test_registry_for(dir.path());
        let session = test_session("overlap");
        ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": session,
                    "start_line": 1,
                    "end_line": 4,
                }),
            )
            .await
            .unwrap();
        let wider = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": dir.path().to_string_lossy(),
                    "_session": session,
                    "start_line": 1,
                    "end_line": 8,
                }),
            )
            .await
            .unwrap();
        // Overlapping read: the new lines (5-8) are served, the overlap is
        // referenced instead of repeated.
        assert!(wider.get("unchanged_since_read").is_some(), "{}", wider);
        let content = wider["content"].as_str().unwrap();
        assert!(content.contains("8 pub fn beta"), "{content}");
        assert!(
            !content.contains("2 #[derive"),
            "overlap re-served: {content}"
        );
    }

    /// §6.6 overlay (cross-lane contract E→F): a read attaches the last-run
    /// check's diagnostics for the requested range, filtered by file and
    /// line, and emits nothing when none match.
    #[tokio::test]
    async fn test_read_overlay_attaches_range_diagnostics_from_last_check() {
        let dir = tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let path = write_sample(&root, "sample.rs");
        let session = test_session("overlay");
        let storage = crate::cli::run::diagnostics::storage_for(&root);
        crate::cli::run::diagnostics::record(
            &storage,
            &[
                crate::cli::run::diagnostics::Diagnostic {
                    file: path.clone(),
                    line: 1,
                    severity: crate::cli::run::diagnostics::Severity::Error,
                    code: Some("E0001".to_string()),
                    message: "first line error".to_string(),
                },
                crate::cli::run::diagnostics::Diagnostic {
                    file: path.clone(),
                    line: 20,
                    severity: crate::cli::run::diagnostics::Severity::Warning,
                    code: None,
                    message: "out of requested range".to_string(),
                },
            ],
        )
        .unwrap();

        let registry = test_registry_for(&root);
        let result = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": root.to_string_lossy(),
                    "_session": session,
                    "start_line": 1,
                    "end_line": 4,
                }),
            )
            .await
            .unwrap();
        let diagnostics = result["diagnostics"]
            .as_array()
            .expect("overlay must attach diagnostics for the requested range");
        assert_eq!(diagnostics.len(), 1, "{result}");
        assert_eq!(diagnostics[0]["line"], 1);
        assert_eq!(diagnostics[0]["code"], "E0001");
        assert_eq!(diagnostics[0]["severity"], "error");

        // A range with no stored hits attaches nothing — sparse, zero bytes.
        let wide = ReadFileHandler
            .execute(
                &registry,
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "project_path": root.to_string_lossy(),
                    "_session": test_session("overlay-wide"),
                    "start_line": 10,
                    "end_line": 14,
                }),
            )
            .await
            .unwrap();
        assert!(wide.get("diagnostics").is_none(), "{wide}");
    }
}
