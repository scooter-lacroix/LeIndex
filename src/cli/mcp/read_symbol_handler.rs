use super::helpers::{byte_range_to_line_range, extract_bool, extract_string, extract_usize};
use super::protocol::JsonRpcError;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::parse::parallel::ParallelParser;
use crate::storage::{CatalogReader, CatalogSymbol};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
                "token_budget": { "type": "integer", "default": 8000 }
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

        let db_path = live.storage().join("leindex.db");
        if db_path.is_file() {
            if let Ok(Some(catalog)) = CatalogReader::open(&db_path, live.root()).await {
                if let Ok(symbols) = catalog.find_symbol(&symbol, file.as_deref()).await {
                    if let Some(mut node) = symbols.into_iter().next() {
                        node.file_path = live
                            .file(&node.file_path.to_string_lossy())
                            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
                        let bytes = read_live_bytes(node.file_path.clone()).await?;
                        if catalog_is_fresh(&catalog, &node.file_path, &bytes).await {
                            let (dependencies, pdg_status) =
                                resident_dependencies(registry, &live, &node, include_dependencies)
                                    .await;
                            return symbol_response(
                                node,
                                bytes,
                                token_budget,
                                false,
                                dependencies,
                                pdg_status,
                            );
                        }
                    }
                }
            }
        }

        let file = file.ok_or_else(|| {
            JsonRpcError::invalid_params(
                "file_path is required when the catalog has no fresh symbol match",
            )
        })?;
        let parsed = parse_live_file(file).await?;
        let node = parsed
            .symbols
            .into_iter()
            .find(|node| node.symbol_name.eq_ignore_ascii_case(&symbol))
            .ok_or_else(|| {
                JsonRpcError::invalid_params(format!(
                    "Symbol '{}' not found in live source",
                    symbol
                ))
            })?;
        let (dependencies, pdg_status) =
            resident_dependencies(registry, &live, &node, include_dependencies).await;
        symbol_response(
            node,
            parsed.bytes,
            token_budget,
            true,
            dependencies,
            pdg_status,
        )
    }
}

fn symbol_response(
    node: CatalogSymbol,
    bytes: Vec<u8>,
    token_budget: usize,
    symbol_index_miss: bool,
    dependencies: Vec<Value>,
    pdg_status: &'static str,
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
        "callers": [],
        "callees": [],
        "dependencies": dependencies,
        "symbol_index_miss": symbol_index_miss,
        "source_freshness": "live",
        "pdg_status": pdg_status
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
                node_type: if signature.is_method {
                    "method"
                } else {
                    "function"
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

async fn resident_dependencies(
    registry: &Arc<ProjectRegistry>,
    live: &LiveProject,
    node: &CatalogSymbol,
    include_dependencies: bool,
) -> (Vec<Value>, &'static str) {
    if !include_dependencies {
        return (Vec::new(), "not_loaded");
    }
    let Some(handle) = registry.try_get_loaded(live.root()).await else {
        return (Vec::new(), "not_loaded");
    };
    let guard = handle.read().await;
    let Some(pdg) = guard.pdg() else {
        return (Vec::new(), "not_loaded");
    };
    let node_id = pdg
        .find_by_symbol(&node.node_id)
        .or_else(|| pdg.find_by_symbol(&node.symbol_name));
    let Some(node_id) = node_id else {
        return (Vec::new(), "fresh");
    };
    let dependencies = pdg
        .neighbors(node_id)
        .iter()
        .filter_map(|&id| {
            let dependency = pdg.get_node(id)?;
            let file = live.file(&dependency.file_path).ok()?;
            Some(serde_json::json!({
                "name": dependency.name,
                "type": super::helpers::node_type_str(&dependency.node_type),
                "file": file,
            }))
        })
        .take(20)
        .collect();
    (dependencies, "fresh")
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
