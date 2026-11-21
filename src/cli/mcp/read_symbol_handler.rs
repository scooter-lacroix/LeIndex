use super::helpers::{
    byte_range_to_line_range, extract_bool, extract_string, extract_usize, get_direct_callees,
};
use super::protocol::JsonRpcError;
use super::read_paths::{self, Fragment};
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
        "leindex_read_symbol"
    }
    pub fn title(&self) -> &str {
        "LeIndex [Read Symbol]"
    }
    pub fn description(&self) -> &str {
        "PRIMARY symbol reader — returns exactly the item (fn/struct/impl/enum/trait) with its doc comment and attributes, nothing else."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "symbol": { "type": "string", "description": "Symbol name to read source for" },
                "file_path": { "type": "string", "description": "Optional file disambiguator; accepts handles (path#symbol, path#L600-800 — path is then the file, fragment the target)" },
                "project_path": { "type": "string", "description": "Project directory" },
                "include_dependencies": { "type": "boolean", "default": false, "description": "Include PDG callers/callees (default off: the item itself is the answer)" },
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

        // Handle form (§4.1): `path#symbol` in file_path — the path becomes
        // the file hint, the fragment the target symbol (a #L range is a
        // read_file concern and is ignored here).
        let (file_hint, symbol) = match file_hint.and_then(|raw| {
            read_paths::split_read_fragment(raw)
                .1
                .map(|fragment| (raw.to_string(), fragment))
        }) {
            Some((raw, fragment)) => match read_paths::classify_fragment(&fragment) {
                Fragment::Symbol(target) => {
                    let (file_part, _) = read_paths::split_read_fragment(&raw);
                    (Some(file_part), target)
                }
                Fragment::LineRange(_) | Fragment::None => (file_hint.map(str::to_string), symbol),
            },
            None => (file_hint.map(str::to_string), symbol),
        };
        let file_hint = file_hint.as_deref();

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
        let (file, source_stale) =
            match catalog_lookup(registry, &live, &symbol, file.as_deref()).await? {
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
                    return symbol_response(
                        node,
                        bytes,
                        token_budget,
                        false,
                        include_dependencies,
                        relations,
                        budget,
                    );
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
        symbol_response(
            node,
            parsed.bytes,
            token_budget,
            true,
            include_dependencies,
            relations,
            budget,
        )
    }
}

enum CatalogLookup {
    Fresh { node: CatalogSymbol, bytes: Vec<u8> },
    Stale { file: PathBuf },
    Miss,
}

async fn catalog_lookup(
    registry: &Arc<ProjectRegistry>,
    live: &LiveProject,
    symbol: &str,
    file: Option<&Path>,
) -> Result<CatalogLookup, JsonRpcError> {
    // D6: the resident PDG is the graph store — resolve symbols against it
    // first; without a resident project, resolve against the published Pdg
    // layer (the unhydrated fast path); the SQL catalog remains the legacy
    // fallback for pre-generation stores (and always serves the freshness
    // check below, since `indexed_files` survives the graph flip).
    let db_path = live.catalog_db();
    let mut graph_symbols = match registry.try_get_loaded(live.root()).await {
        Some(handle) => {
            let guard = handle.read().await;
            guard.pdg().map(|pdg| {
                crate::storage::catalog::graph::find_symbol(pdg, live.root(), symbol, file)
            })
        }
        None => None,
    };
    if graph_symbols.as_ref().is_none_or(Vec::is_empty) {
        let layer_symbols =
            crate::storage::catalog::layer::find_symbol(live.storage(), live.root(), symbol, file);
        if !layer_symbols.is_empty() {
            graph_symbols = Some(layer_symbols);
        }
    }

    let catalog = if db_path.is_file() {
        CatalogReader::open(&db_path, live.root())
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let symbols = match graph_symbols {
        Some(symbols) => symbols,
        None => match &catalog {
            Some(catalog) => catalog.find_symbol(symbol, file).await.unwrap_or_default(),
            None => Vec::new(),
        },
    };
    // A symbol can live in several files (overloads, re-exports, shadowing).
    // Pick the best-ranked candidate instead of blindly taking the first row
    // so a struct definition beats a same-named variable in another file.
    let Some(mut node) = symbols.into_iter().min_by_key(|s| symbol_rank(s, symbol)) else {
        return Ok(CatalogLookup::Miss);
    };
    node.file_path = live
        .file(&node.file_path.to_string_lossy())
        .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    let bytes = read_live_bytes(node.file_path.clone()).await?;
    let fresh = match &catalog {
        Some(catalog) => catalog_is_fresh(catalog, &node.file_path, &bytes).await,
        // No freshness record exists; treat as stale so the caller parses
        // live instead of trusting an unverifiable graph row.
        None => false,
    };
    if fresh {
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

/// Rank candidate symbols for disambiguation. Lower tuple values win:
///
/// 1. Exactness: a case-sensitive name/qualified-name match beats a
///    case-insensitive one.
/// 2. Type: definition-like nodes (class/struct, then function/method) beat
///    variables and modules, so a `struct Storage` is preferred over a
///    same-named CLI variable.
/// 3. Size: a larger byte range is treated as the real definition over a
///    reference or re-export with the same name.
fn symbol_rank(node: &CatalogSymbol, symbol: &str) -> (u8, u8, std::cmp::Reverse<usize>) {
    let exact = if node.symbol_name == symbol || node.qualified_name == symbol {
        0
    } else {
        1
    };
    let type_rank = match node.node_type.as_str() {
        "class" => 0,
        "function" | "method" => 1,
        "module" => 2,
        _ => 3,
    };
    let size = node.byte_range.1.saturating_sub(node.byte_range.0);
    (exact, type_rank, std::cmp::Reverse(size))
}

fn find_live_symbol(parsed: &LiveParse, symbol: &str) -> Option<CatalogSymbol> {
    parsed
        .symbols
        .iter()
        .filter(|node| {
            node.symbol_name.eq_ignore_ascii_case(symbol)
                || node.qualified_name.eq_ignore_ascii_case(symbol)
        })
        .min_by_key(|node| symbol_rank(node, symbol))
        .cloned()
}

/// Live source candidates for the fallback scan: git-aware inventory when the
/// tree is a worktree, otherwise a walkdir scan of the absolute project root.
///
/// A listing that FAILS (git errored, spawn failure) is surfaced as an error —
/// it must never degrade to an empty list, which would report a present symbol
/// as "not found" on the strength of a listing that never happened. The two
/// sources agree by construction: absolute paths, symlinks NOT followed, and
/// SKIP_DIRS-pruned traversal.
fn live_source_inventory(root: &Path) -> Result<Vec<PathBuf>, crate::cli::git::GitInventoryError> {
    match crate::cli::git::source_inventory(root) {
        Ok(paths) => Ok(paths),
        Err(crate::cli::git::GitInventoryError::NotRepository) => Ok(walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                let name = entry.file_name().to_string_lossy();
                !crate::skip_dirs::SKIP_DIRS.iter().any(|skip| name == *skip)
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .collect()),
        Err(other) => Err(other),
    }
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
    let candidates = tokio::task::spawn_blocking(move || live_source_inventory(&root))
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("live symbol inventory task failed: {error}"))
        })?
        .map_err(|error| {
            JsonRpcError::internal_error(format!(
                "could not list live source files ({error}); symbol presence is unverified"
            ))
        })?;
    // Content prefilter: only parse files whose bytes mention the symbol, so the
    // bounded parse cap applies to *relevant* files. `inspected` caps the number
    // of files READ (a missing symbol otherwise scans the whole tree); `parsed`
    // caps parsed files. Byte matching via `from_utf8` (no whole-file allocation).
    // Case-insensitive so differently-cased declarations are not skipped
    // (consistent with the eq_ignore_ascii_case catalog fallback).
    let symbol_lower = symbol.to_ascii_lowercase();
    let mut parsed = 0usize;
    // Collect the best-ranked match across *all* scanned files rather than
    // returning the first file that mentions the symbol: a later-sorting file
    // can hold the real definition while an earlier one only shadows or
    // re-exports the name.
    let mut best: Option<(LiveParse, CatalogSymbol)> = None;
    for (inspected, candidate) in candidates.into_iter().enumerate() {
        // §4.1 regression fix: this loop used to stop at 200 inspected / 20
        // parsed files and then report "Symbol not found" — a FALSE negative,
        // because a mention-heavy caller file can consume the parsed cap
        // before the definition file is reached (that is exactly how a
        // private `fn render_read_file` in an 1,859-line file failed during
        // recon). A missing symbol may only be claimed after the inventory
        // is exhausted, so the only cap left is the inventory itself
        // (bounded by the repo; MAX_INVENTORY_FILES guards pathological
        // trees). The content prefilter keeps the parse count proportional
        // to files that actually mention the symbol.
        if inspected >= MAX_INVENTORY_FILES {
            break;
        }
        let Ok(bytes) = read_live_bytes(candidate.clone()).await else {
            continue;
        };
        if !std::str::from_utf8(&bytes)
            .map(|content| content.to_ascii_lowercase().contains(&symbol_lower))
            .unwrap_or(false)
        {
            continue;
        }
        parsed += 1;
        let Ok(live_parsed) = parse_live_file(candidate).await else {
            continue;
        };
        let Some(node) = find_live_symbol(&live_parsed, symbol) else {
            continue;
        };
        let is_better = best
            .as_ref()
            .is_none_or(|(_, current)| symbol_rank(&node, symbol) < symbol_rank(current, symbol));
        if is_better {
            best = Some((live_parsed, node));
        }
    }
    let Some((live_parsed, node)) = best else {
        return Err(JsonRpcError::invalid_params(format!(
            "Symbol '{}' not found: scanned the live source inventory (bounded at {} files), parsed {} files that mentioned it — no definition found",
            symbol, MAX_INVENTORY_FILES, parsed
        )));
    };
    Ok((live_parsed, node))
}

/// Upper bound on files inspected by the no-hint inventory scan. The scan
/// reads at most this many files before reporting an honest miss.
const MAX_INVENTORY_FILES: usize = 2000;

fn symbol_response(
    node: CatalogSymbol,
    bytes: Vec<u8>,
    token_budget: usize,
    symbol_index_miss: bool,
    include_dependencies: bool,
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
    // The item's own documentation AND attributes: contiguous ///, //! and
    // #[...] lines above the item, with blank separators allowed between the
    // doc block and the attributes. The old walk saw /// only, so a
    // `#[derive(...)]` above the fn was silently dropped from the answer.
    let preceding: Vec<&str> = content[..start]
        .lines()
        .rev()
        .take_while(|line| {
            let trimmed = line.trim_start();
            trimmed.is_empty()
                || trimmed.starts_with("///")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("#[")
                || trimmed.starts_with("#![")
        })
        .collect();
    let doc_comment: Vec<&str> = preceding
        .iter()
        .rev()
        .copied()
        .skip_while(|line| line.trim().is_empty())
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("///") || trimmed.starts_with("//!")
        })
        .map(|line| line.trim_start())
        .collect();
    let attributes: Vec<&str> = preceding
        .iter()
        .rev()
        .copied()
        .skip_while(|line| line.trim().is_empty())
        .filter(|line| line.trim_start().starts_with("#["))
        .map(|line| line.trim_start())
        .collect();
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
        "doc_comment": if doc_comment.is_empty() { None } else { Some(doc_comment.join("\n")) },
        "attributes": if attributes.is_empty() { None } else { Some(attributes.join("\n")) },
        "source": source,
        "source_truncated": end - start > token_budget * 4,
        "_source_char_budget": token_budget * 4,
        // Exactness (§4.1): the default answer is the item itself. Callers/
        // callees are attached only when explicitly requested via
        // include_dependencies.
        "callers": if include_dependencies { relations.callers } else { Vec::new() },
        "callees": if include_dependencies { relations.callees } else { Vec::new() },
        "dependencies": if include_dependencies { relations.dependencies } else { Vec::new() },
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
                    };
                }
            }
        }
        None => {
            return ResidentRelations {
                callers: Vec::new(),
                callees: Vec::new(),
                dependencies: Vec::new(),
                pdg_status: "not_loaded",
            };
        }
    };

    // `include_dependencies=true` is an explicit request for PDG-backed
    // relationships. Hydrate the active immutable generation when the
    // resident handle has not loaded it yet; never silently return an empty
    // dependency list for a requested enrichment layer.
    if include_dependencies {
        let mut guard = handle.write().await;
        // Graph-only hydration (see ensure_pdg_loaded_graph_only): relations
        // never need the search engine.
        if guard.pdg().is_none() && guard.ensure_pdg_loaded_graph_only().is_err() {
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
    let callees = relation_nodes_to_json(pdg, live, get_direct_callees(pdg, node_id));
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
    // Neighbors yields one entry per edge; the same related node can reach
    // the anchor through several edge types and must render once (N-01).
    let mut seen = std::collections::HashSet::new();
    node_ids
        .into_iter()
        .filter(|id| seen.insert(*id))
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

    fn catalog_symbol(name: &str, node_type: &str, byte_range: (usize, usize)) -> CatalogSymbol {
        CatalogSymbol {
            node_id: format!("{name}:{byte_range:?}"),
            symbol_name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: std::path::PathBuf::from(format!("/project/{name}.rs")),
            language: "rust".to_string(),
            node_type: node_type.to_string(),
            complexity: 0,
            byte_range,
        }
    }

    #[test]
    fn test_find_live_symbol_prefers_struct_over_variable() {
        // Regression: a same-named CLI variable must not shadow the struct
        // definition (read_symbol previously returned the first match).
        let parsed = LiveParse {
            bytes: Vec::new(),
            symbols: vec![
                catalog_symbol("Storage", "variable", (10, 20)),
                catalog_symbol("Storage", "class", (30, 90)),
            ],
        };
        let found = find_live_symbol(&parsed, "Storage").unwrap();
        assert_eq!(found.node_type, "class", "struct/class must beat variable");
    }

    #[test]
    fn test_find_live_symbol_prefers_exact_over_case_insensitive() {
        let parsed = LiveParse {
            bytes: Vec::new(),
            symbols: vec![
                catalog_symbol("storage", "function", (10, 30)),
                catalog_symbol("Storage", "class", (40, 60)),
            ],
        };
        let found = find_live_symbol(&parsed, "Storage").unwrap();
        assert_eq!(found.symbol_name, "Storage", "exact case match must win");
    }

    #[test]
    fn test_find_live_symbol_prefers_larger_range_within_same_type() {
        let parsed = LiveParse {
            bytes: Vec::new(),
            symbols: vec![
                catalog_symbol("helper", "function", (5, 9)),
                catalog_symbol("helper", "function", (20, 60)),
            ],
        };
        let found = find_live_symbol(&parsed, "helper").unwrap();
        assert_eq!(found.byte_range, (20, 60), "definition-like range must win");
    }

    #[tokio::test]
    async fn test_find_live_symbol_in_inventory_picks_best_across_files() {
        // A variable in an earlier-sorting file must not win over the struct
        // definition in a later file.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a_cli.rs"),
            "pub const Storage: usize = 42;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b_storage.rs"),
            "pub struct Storage { field: u32 }\n",
        )
        .unwrap();
        let live = LiveProject::resolve(&dir.path().to_string_lossy()).unwrap();
        let (_parsed, node) = find_live_symbol_in_inventory(&live, "Storage")
            .await
            .expect("symbol must be found");
        assert_eq!(node.node_type, "class");
        assert!(node.file_path.ends_with("b_storage.rs"));
    }

    #[test]
    fn test_live_source_inventory_failure_is_not_an_empty_list() {
        // A listing failure must surface as an error, not as an empty inventory
        // that reads as "symbol not found".
        let dir = tempdir().unwrap();
        let file = dir.path().join("phantom.rs");
        std::fs::write(&file, "pub fn target() {}\n").unwrap();
        let err = live_source_inventory(&file).unwrap_err();
        assert!(matches!(err, crate::cli::git::GitInventoryError::Io(_)));
    }

    #[tokio::test]
    async fn test_read_symbol_returns_exactly_item_with_doc_and_attributes() {
        // §4.1 exactness: the answer is the item + its doc + its attributes,
        // nothing else — no sibling items, no caller lists by default.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("thing.rs"),
            "/// Builds the thing.\n///\n/// Multi-paragraph.\n#[inline]\n#[must_use]\nfn build_thing(x: u32) -> u32 {\n    x * 2\n}\n\npub fn unrelated() {}\n",
        )
        .unwrap();
        let registry = test_registry_for(dir.path());
        let result = ReadSymbolHandler
            .execute(
                &registry,
                serde_json::json!({
                    "symbol": "build_thing",
                    "project_path": dir.path().to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["symbol"], "build_thing");
        let source = result["source"].as_str().unwrap();
        assert!(source.starts_with("fn build_thing"), "{source}");
        assert!(source.contains("x * 2"), "{source}");
        assert!(!source.contains("unrelated"), "{source}");
        assert_eq!(
            result["doc_comment"].as_str().unwrap(),
            "/// Builds the thing.\n///\n/// Multi-paragraph."
        );
        assert_eq!(
            result["attributes"].as_str().unwrap(),
            "#[inline]\n#[must_use]"
        );
        assert_eq!(
            result["callers"].as_array().unwrap().len(),
            0,
            "no dependency lists unless requested: {result}"
        );
        assert_eq!(result["callees"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_read_symbol_returns_method_inside_impl_not_whole_impl() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("machine.rs"),
            "pub struct Machine;\n\nimpl Machine {\n    /// Runs one step.\n    pub fn step(&self) -> u32 {\n        1\n    }\n\n    pub fn stop(&self) {\n        // nothing\n    }\n}\n",
        )
        .unwrap();
        let registry = test_registry_for(dir.path());
        let result = ReadSymbolHandler
            .execute(
                &registry,
                serde_json::json!({
                    "symbol": "step",
                    "project_path": dir.path().to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        let source = result["source"].as_str().unwrap();
        assert!(source.contains("fn step"), "{source}");
        assert_eq!(
            result["doc_comment"].as_str().unwrap(),
            "/// Runs one step.",
            "the method's doc comment comes back with the item"
        );
        assert!(!source.contains("fn stop"), "whole impl leaked: {source}");
        assert!(!source.contains("struct Machine"), "{source}");
    }

    #[tokio::test]
    async fn test_read_symbol_finds_fn_in_nested_module() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("nested.rs"),
            "pub mod inner {\n    /// Deep doc.\n    pub fn deep() -> u32 {\n        7\n    }\n}\n",
        )
        .unwrap();
        let registry = test_registry_for(dir.path());
        let result = ReadSymbolHandler
            .execute(
                &registry,
                serde_json::json!({
                    "symbol": "deep",
                    "project_path": dir.path().to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["symbol"], "deep");
        let source = result["source"].as_str().unwrap();
        assert!(source.contains("fn deep"), "{source}");
        assert_eq!(result["doc_comment"].as_str().unwrap(), "/// Deep doc.");
    }

    #[tokio::test]
    async fn test_read_symbol_finds_private_fn_beyond_mention_heavy_files() {
        // §4.1 regression: the inventory scan used to stop at 20 parsed
        // files and report a FALSE not-found for a definition living in a
        // later-sorting file. The scan must run until the definition is
        // found, only bounded by the inventory itself.
        let dir = tempdir().unwrap();
        for i in 0..25 {
            std::fs::write(
                dir.path().join(format!("aa_mention_{i:02}.rs")),
                format!("pub fn caller_{i}() -> usize {{\n    render_thing(1)\n}}\n"),
            )
            .unwrap();
        }
        std::fs::write(
            dir.path().join("zz_render.rs"),
            "fn render_thing(v: u32) -> usize {\n    v as usize\n}\n",
        )
        .unwrap();
        let registry = test_registry_for(dir.path());
        let result = ReadSymbolHandler
            .execute(
                &registry,
                serde_json::json!({
                    "symbol": "render_thing",
                    "project_path": dir.path().to_string_lossy(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(result["symbol"], "render_thing");
        let source = result["source"].as_str().unwrap();
        assert!(source.contains("v as usize"), "{source}");
    }
}
