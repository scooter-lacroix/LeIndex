use super::helpers::{byte_range_to_line_range, extract_bool, extract_string, extract_usize};
use super::protocol::JsonRpcError;
use super::request_meta::WorkBudget;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::parse::parallel::ParallelParser;
use crate::storage::{CatalogReader, CatalogSymbol};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Handler for LeIndex [read_symbol — targeted symbol source read.
#[derive(Clone)]
pub struct ReadSymbolHandler;

#[allow(missing_docs)]
impl ReadSymbolHandler {
    pub fn name(&self) -> &str {
        "leindex.read-symbol"
    }
    pub fn title(&self) -> &str {
        "LeIndex [Read Symbol]"
    }
    pub fn description(&self) -> &str {
        "PRIMARY symbol reader — returns exact source code with line numbers, doc comments, and compact caller/callee locations."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Symbol name to read source for" },
                "file_path": { "type": "string", "description": "Optional file disambiguator" },
                "project_path": { "type": "string", "description": "Project directory" },
                "include_dependencies": { "type": "boolean", "default": false },
                "token_budget": { "type": "integer", "default": 8000 },
                "max_latency_ms": { "type": "integer", "default": 250, "minimum": 0, "maximum": 60000 },
                "allow_partial": { "type": "boolean", "default": true }
            },
            "required": ["symbol"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let symbol = extract_string(&args, "symbol")?;
        let file_hint = args.get("file_path").and_then(|v| v.as_str());
        let include_dependencies = extract_bool(&args, "include_dependencies", false);
        let token_budget = extract_usize(&args, "token_budget", 8000)?;
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 250)?.min(60000) as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };
        let started = Instant::now();
        let project_path = match args.get("project_path").and_then(|v| v.as_str()) {
            Some(path) => PathBuf::from(path),
            None => registry.default_project_path().await?,
        };
        let live = LiveProject::resolve(&project_path.to_string_lossy()).map_err(|e| {
            JsonRpcError::invalid_params(format!(
                "Cannot resolve project_path '{}': {}",
                project_path.display(),
                e
            ))
        })?;
        let file = file_hint
            .map(|raw| live.file(raw))
            .transpose()
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
        let (file, source_stale) = match catalog_lookup(&live, &symbol, file.as_deref()).await? {
            CatalogLookup::Fresh { node, bytes } => {
                let relations = resident_relations(
                    registry,
                    &live,
                    &node,
                    include_dependencies,
                    false,
                    budget,
                    started,
                )
                .await;
                return symbol_response(node, bytes, token_budget, false, relations, budget);
            }
            CatalogLookup::Stale { file } => (Some(file), true),
            CatalogLookup::Miss => (file, false),
        };

        let (parsed, node) = match file {
            Some(file) => parse_live_symbol(file, &symbol).await?,
            None => find_live_symbol_in_inventory(&live, &symbol).await?,
        };
        let relations = resident_relations(
            registry,
            &live,
            &node,
            include_dependencies,
            source_stale,
            budget,
            started,
        )
        .await;
        symbol_response(node, parsed.bytes, token_budget, true, relations, budget)
    }
}

enum CatalogLookup {
    Fresh { node: CatalogSymbol, bytes: Vec<u8> },
    Stale { file: PathBuf },
    Miss,
}

async fn catalog_lookup(
    live: &LiveProject,
    symbol: &str,
    file: Option<&Path>,
) -> Result<CatalogLookup, JsonRpcError> {
    let db_path = live.active_storage().join("leindex.db");
    if !db_path.is_file() {
        return Ok(CatalogLookup::Miss);
    }
    let Ok(Some(catalog)) = CatalogReader::open(&db_path, live.root()).await else {
        return Ok(CatalogLookup::Miss);
    };
    let Ok(symbols) = catalog.find_symbol(symbol, file).await else {
        return Ok(CatalogLookup::Miss);
    };
    let Some(mut node) = symbols.into_iter().next() else {
        return Ok(CatalogLookup::Miss);
    };
    node.file_path = live
        .file(&node.file_path.to_string_lossy())
        .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let bytes = read_live_bytes(node.file_path.clone()).await?;
    if catalog_is_fresh(&catalog, &node.file_path, &bytes).await {
        Ok(CatalogLookup::Fresh { node, bytes })
    } else {
        // A stale catalog still supplies a vetted in-root source candidate;
        // parse it live instead of hydrating a PDG.
        Ok(CatalogLookup::Stale {
            file: node.file_path,
        })
    }
}

async fn parse_live_symbol(
    file: PathBuf,
    symbol: &str,
) -> Result<(LiveParse, CatalogSymbol), JsonRpcError> {
    let parsed = parse_live_file(file).await?;
    let node = find_live_symbol(&parsed, symbol).ok_or_else(|| {
        JsonRpcError::invalid_params(format!("Symbol '{}' not found in live source", symbol))
    })?;
    Ok((parsed, node))
}

fn find_live_symbol(parsed: &LiveParse, symbol: &str) -> Option<CatalogSymbol> {
    parsed
        .symbols
        .iter()
        .find(|node| {
            node.symbol_name.eq_ignore_ascii_case(symbol)
                || node.qualified_name.eq_ignore_ascii_case(symbol)
        })
        .cloned()
}

async fn find_live_symbol_in_inventory(
    live: &LiveProject,
    symbol: &str,
) -> Result<(LiveParse, CatalogSymbol), JsonRpcError> {
    // A catalog miss without a file hint should still be useful. Git supplies
    // an ignore-aware candidate list; we prefilter by content to find relevant
    // files before applying a bounded parse cap so a symbol in a later-sorting
    // file is not reported absent.
    let root = live.root().to_path_buf();
    let candidates =
        tokio::task::spawn_blocking(move || match crate::cli::git::source_inventory(&root) {
            Ok(paths) => paths,
            Err(crate::cli::git::GitInventoryError::NotRepository) => walkdir::WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    let name = entry.file_name().to_string_lossy();
                    !crate::cli::skip_dirs::SKIP_DIRS
                        .iter()
                        .any(|skip| name == *skip)
                })
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().to_path_buf())
                .collect(),
            Err(_) => Vec::new(),
        })
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("live symbol inventory failed: {error}"))
        })?;
    // Content prefilter: only parse files whose bytes mention the symbol, so the
    // bounded parse cap applies to *relevant* files rather than an arbitrary
    // alphabetical slice — a symbol in a later-sorting file is still found. A
    // path-name prefilter would be wrong (symbols are not named after their files).
    // Content prefilter: only parse files whose bytes mention the symbol, so the
    // bounded parse cap applies to *relevant* files. `inspected` caps the number
    // of files READ (a missing symbol otherwise scans the whole tree); `parsed`
    // caps parsed files. Byte matching via `from_utf8` (no whole-file allocation).
    let mut parsed = 0usize;
    let mut inspected = 0usize;
    for candidate in candidates {
        if parsed >= 20 || inspected >= 200 {
            break;
        }
        inspected += 1;
        let Ok(bytes) = read_live_bytes(candidate.clone()).await else {
            continue;
        };
        if !std::str::from_utf8(&bytes)
            .map(|content| content.contains(symbol))
            .unwrap_or(false)
        {
            continue;
        }
        parsed += 1;
        let Ok(live_parsed) = parse_live_file(candidate).await else {
            continue;
        };
        if let Some(node) = find_live_symbol(&live_parsed, symbol) {
            return Ok((live_parsed, node));
        }
    }
    Err(JsonRpcError::invalid_params(format!(
        "Symbol '{}' not found in the first 20 live source candidates",
        symbol
    )))
}

fn symbol_response(
    node: CatalogSymbol,
    bytes: Vec<u8>,
    token_budget: usize,
    symbol_index_miss: bool,
    relations: ResidentRelations,
    budget: WorkBudget,
) -> Result<Value, JsonRpcError> {
    let (start, end) = node.byte_range;
    let content = std::str::from_utf8(&bytes).map_err(|e| {
        JsonRpcError::invalid_params(format!(
            "Source '{}' is not UTF-8: {}",
            node.file_path.display(),
            e
        ))
    })?;
    if start > end
        || end > bytes.len()
        || !content.is_char_boundary(start)
        || !content.is_char_boundary(end)
    {
        return Err(JsonRpcError::invalid_params(format!(
            "Indexed byte range for '{}' is outside live UTF-8 source boundaries",
            node.symbol_name
        )));
    }
    let (line_start, line_end) = byte_range_to_line_range(content, node.byte_range);
    let source: String = content[start..end].chars().take(token_budget * 4).collect();
    let doc_comment = content[..start]
        .lines()
        .rev()
        .take_while(|line| line.trim().is_empty() || line.trim_start().starts_with("///"))
        .filter(|line| line.trim_start().starts_with("///"))
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "symbol": node.symbol_name,
        "qualified_name": node.qualified_name,
        "node_id": node.node_id,
        "type": node.node_type,
        "file": node.file_path,
        "language": node.language,
        "complexity": node.complexity,
        "line_start": line_start,
        "line_end": line_end,
        "doc_comment": if doc_comment.is_empty() { None } else { Some(doc_comment.into_iter().rev().collect::<Vec<_>>().join("\n")) },
        "source": source,
        "source_truncated": end - start > token_budget * 4,
        "_source_char_budget": token_budget * 4,
        "callers": relations.callers,
        "callees": relations.callees,
        "dependencies": relations.dependencies,
        "symbol_index_miss": symbol_index_miss,
        "source_freshness": "live",
        "pdg_status": relations.pdg_status,
        "retrieval": {
            "tfidf_status": "not_used_exact",
            "neural_status": "not_used_exact",
            "partial": matches!(relations.pdg_status, "partial" | "stale"),
            "max_latency_ms": budget.max_latency_ms,
            "allow_partial": budget.allow_partial
        }
    }))
}

pub(crate) async fn catalog_is_fresh(catalog: &CatalogReader, file: &Path, bytes: &[u8]) -> bool {
    catalog
        .indexed_file_hash(file)
        .await
        .ok()
        .flatten()
        .is_some_and(|hash| hash == blake3::hash(bytes).to_hex().to_string())
}

pub(crate) struct LiveParse {
    pub bytes: Vec<u8>,
    pub symbols: Vec<CatalogSymbol>,
}

pub(crate) async fn read_live_bytes(path: PathBuf) -> Result<Vec<u8>, JsonRpcError> {
    tokio::task::spawn_blocking(move || std::fs::read(&path).map_err(|e| (path, e)))
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("source read task failed: {}", e)))?
        .map_err(|(path, e)| {
            JsonRpcError::invalid_params(format!("Cannot read source '{}': {}", path.display(), e))
        })
}

pub(crate) async fn parse_live_file(path: PathBuf) -> Result<LiveParse, JsonRpcError> {
    tokio::task::spawn_blocking(move || {
        let parsed = ParallelParser::new()
            .with_max_threads(1)
            .parse_files(vec![path.clone()])
            .into_iter()
            .next()
            .ok_or_else(|| JsonRpcError::invalid_params("Live parser returned no result"))?;
        let language = parsed.language.unwrap_or_else(|| "unknown".to_string());
        let bytes = parsed.source_bytes.ok_or_else(|| {
            JsonRpcError::invalid_params(format!(
                "Cannot parse live source '{}': {}",
                path.display(),
                parsed
                    .error
                    .unwrap_or_else(|| "unknown parser error".to_string())
            ))
        })?;
        let symbols = parsed
            .signatures
            .into_iter()
            .map(|signature| CatalogSymbol {
                node_id: format!("{}:{}", path.display(), signature.qualified_name),
                symbol_name: signature.name,
                qualified_name: signature.qualified_name,
                file_path: path.clone(),
                language: language.clone(),
                node_type: match signature.return_type.as_deref() {
                    Some("module") => "module",
                    Some("enum_variant") => "variable",
                    Some("enum") | Some("trait") => "class",
                    Some(value) if value.starts_with("struct") => "class",
                    _ if signature.is_method => "method",
                    _ => "function",
                }
                .to_string(),
                complexity: signature.cyclomatic_complexity,
                byte_range: signature.byte_range,
            })
            .collect();
        Ok(LiveParse { bytes, symbols })
    })
    .await
    .map_err(|e| JsonRpcError::internal_error(format!("live parser task failed: {}", e)))?
}

struct ResidentRelations {
    callers: Vec<Value>,
    callees: Vec<Value>,
    dependencies: Vec<Value>,
    pdg_status: &'static str,
}

async fn resident_relations(
    registry: &Arc<ProjectRegistry>,
    live: &LiveProject,
    node: &CatalogSymbol,
    include_dependencies: bool,
    source_stale: bool,
    budget: WorkBudget,
    started: Instant,
) -> ResidentRelations {
    if source_stale {
        return ResidentRelations {
            callers: Vec::new(),
            callees: Vec::new(),
            dependencies: Vec::new(),
            pdg_status: "stale",
        };
    }

    let handle = match registry.try_get_loaded(live.root()).await {
        Some(handle) => handle,
        None if include_dependencies => {
            let project = live.root().to_string_lossy().into_owned();
            match registry.get_or_load(Some(&project)).await {
                Ok(handle) => handle,
                Err(_) => {
                    return ResidentRelations {
                        callers: Vec::new(),
                        callees: Vec::new(),
                        dependencies: Vec::new(),
                        pdg_status: "partial",
                    }
                }
            }
        }
        None => {
            return ResidentRelations {
                callers: Vec::new(),
                callees: Vec::new(),
                dependencies: Vec::new(),
                pdg_status: "not_loaded",
            }
        }
    };

    // `include_dependencies=true` is an explicit request for PDG-backed
    // relationships. Hydrate the active immutable generation when the
    // resident handle has not loaded it yet; never silently return an empty
    // dependency list for a requested enrichment layer.
    if include_dependencies {
        let mut guard = handle.write().await;
        if guard.pdg().is_none() && guard.load_from_storage().is_err() {
            return ResidentRelations {
                callers: Vec::new(),
                callees: Vec::new(),
                dependencies: Vec::new(),
                pdg_status: "partial",
            };
        }
    }

    let guard = handle.read().await;
    let Some(pdg) = guard.pdg() else {
        return ResidentRelations {
            callers: Vec::new(),
            callees: Vec::new(),
            dependencies: Vec::new(),
            pdg_status: "not_loaded",
        };
    };
    let node_id = pdg
        .find_by_symbol(&node.node_id)
        .or_else(|| pdg.find_by_symbol(&node.symbol_name));
    let Some(node_id) = node_id else {
        return ResidentRelations {
            callers: Vec::new(),
            callees: Vec::new(),
            dependencies: Vec::new(),
            pdg_status: "fresh",
        };
    };
    if budget.elapsed(started) {
        return ResidentRelations {
            callers: Vec::new(),
            callees: Vec::new(),
            dependencies: Vec::new(),
            pdg_status: "partial",
        };
    }
    let callees = relation_nodes_to_json(pdg, live, pdg.neighbors(node_id));
    let callers =
        relation_nodes_to_json(pdg, live, super::helpers::get_direct_callers(pdg, node_id));
    ResidentRelations {
        dependencies: if include_dependencies {
            callees.clone()
        } else {
            Vec::new()
        },
        callees,
        callers,
        pdg_status: if budget.elapsed(started) {
            "partial"
        } else {
            "fresh"
        },
    }
}

fn relation_nodes_to_json(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    live: &LiveProject,
    node_ids: impl IntoIterator<Item = crate::graph::pdg::NodeId>,
) -> Vec<Value> {
    node_ids
        .into_iter()
        .filter_map(|id| {
            let node = pdg.get_node(id)?;
            let file = live.file(&node.file_path).ok()?;
            Some(serde_json::json!({
                "name": node.name,
                "type": super::helpers::node_type_str(&node.node_type),
                "file": file,
            }))
        })
        .take(20)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_read_symbol_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "symbol": "my_func" });
        assert!(ReadSymbolHandler.execute(&registry, args).await.is_err());
    }
}
