use super::helpers::{
    extract_bool, extract_string, extract_usize, get_direct_callers, node_type_str,
};
use super::protocol::JsonRpcError;
use super::read_symbol_handler::{catalog_is_fresh, parse_live_file, read_live_bytes};
use super::request_meta::WorkBudget;
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::storage::{CatalogReader, CatalogSymbol};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Handler for LeIndex [file_summary — structured file analysis replacing Read.
#[derive(Clone)]
pub struct FileSummaryHandler;

#[allow(missing_docs)]
impl FileSummaryHandler {
    pub fn name(&self) -> &str {
        "leindex.file-summary"
    }
    pub fn title(&self) -> &str {
        "LeIndex [File Summary]"
    }
    pub fn description(&self) -> &str {
        "File overview: symbol inventory, complexity scores, cross-file dependencies, and module role."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "File to analyze" },
                "project_path": { "type": "string", "description": "Project directory" },
                "token_budget": { "type": "integer", "default": 1000 },
                "include_source": { "type": "boolean", "default": false },
                "focus_symbol": { "type": "string" },
                "max_latency_ms": { "type": "integer", "default": 250, "minimum": 0, "maximum": 60000 },
                "allow_partial": { "type": "boolean", "default": true }
            },
            "required": ["file_path"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let requested_file = extract_string(&args, "file_path")?;
        let include_source = extract_bool(&args, "include_source", false);
        let focus_symbol = args.get("focus_symbol").and_then(|v| v.as_str());
        let token_budget = extract_usize(&args, "token_budget", 1000)?;
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
        let file = live
            .file(&requested_file)
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
        let (catalog_response, source_stale) = catalog_file_summary(
            registry,
            &live,
            &file,
            include_source,
            focus_symbol,
            token_budget,
            budget,
            started,
        )
        .await?;
        if let Some(response) = catalog_response {
            return Ok(response);
        }
        let parsed = parse_live_file(file.clone()).await?;
        let (relations, pdg_status) = resident_file_relations(
            registry,
            &live,
            &parsed.symbols,
            source_stale,
            budget,
            started,
        )
        .await;
        file_summary_response(
            &file,
            &parsed.bytes,
            parsed.symbols,
            include_source,
            focus_symbol,
            token_budget,
            true,
            false,
            None,
            &relations,
            pdg_status,
            budget,
        )
    }
}

async fn catalog_file_summary(
    registry: &Arc<ProjectRegistry>,
    live: &LiveProject,
    file: &Path,
    include_source: bool,
    focus_symbol: Option<&str>,
    token_budget: usize,
    budget: WorkBudget,
    started: Instant,
) -> Result<(Option<Value>, bool), JsonRpcError> {
    let db_path = live.active_storage().join("leindex.db");
    if !db_path.is_file() {
        return Ok((None, false));
    }
    let Ok(Some(catalog)) = CatalogReader::open(&db_path, live.root()).await else {
        return Ok((None, false));
    };
    let Ok(mut symbols) = catalog.symbols_in_file(file).await else {
        return Ok((None, false));
    };
    for symbol in &mut symbols {
        symbol.file_path = live
            .file(&symbol.file_path.to_string_lossy())
            .map_err(|e| JsonRpcError::invalid_params(e.to_string()))?;
    }
    if symbols.is_empty() {
        return Ok((None, false));
    }

    let catalog_total = catalog.count_symbols_in_file(file).await.ok();
    let catalog_truncated = catalog_total
        .map(|total| total > symbols.len())
        .unwrap_or(symbols.len() >= 200);
    let bytes = read_live_bytes(file.to_path_buf()).await?;
    if !catalog_is_fresh(&catalog, file, &bytes).await {
        return Ok((None, true));
    }

    let (relations, pdg_status) =
        resident_file_relations(registry, live, &symbols, false, budget, started).await;
    let response = file_summary_response(
        file,
        &bytes,
        symbols,
        include_source,
        focus_symbol,
        token_budget,
        false,
        catalog_truncated,
        catalog_total,
        &relations,
        pdg_status,
        budget,
    )?;
    Ok((Some(response), false))
}

fn file_summary_response(
    file_path: &Path,
    bytes: &[u8],
    symbols: Vec<CatalogSymbol>,
    include_source: bool,
    focus_symbol: Option<&str>,
    token_budget: usize,
    symbol_index_miss: bool,
    catalog_truncated: bool,
    catalog_total: Option<usize>,
    relations: &HashMap<String, Value>,
    pdg_status: &'static str,
    budget: WorkBudget,
) -> Result<Value, JsonRpcError> {
    let content = std::str::from_utf8(bytes).map_err(|e| {
        JsonRpcError::invalid_params(format!(
            "Source '{}' is not UTF-8: {}",
            file_path.display(),
            e
        ))
    })?;
    let mut total_chars = 0usize;
    let char_budget = token_budget * 4;
    let mut shown = Vec::new();
    let mut total = 0usize;
    for symbol in symbols {
        if symbol.symbol_name.is_empty()
            || focus_symbol.is_some_and(|focus| {
                !symbol
                    .symbol_name
                    .to_lowercase()
                    .contains(&focus.to_lowercase())
            })
        {
            continue;
        }
        total += 1;
        if total_chars >= char_budget {
            continue;
        }
        let (start, end) = symbol.byte_range;
        if start > end
            || end > bytes.len()
            || !content.is_char_boundary(start)
            || !content.is_char_boundary(end)
        {
            return Err(JsonRpcError::invalid_params(format!(
                "Indexed byte range for '{}' is outside live UTF-8 source boundaries",
                symbol.symbol_name
            )));
        }
        let relation = relations.get(&symbol.node_id).cloned().unwrap_or_else(|| {
            serde_json::json!({
                "dependencies": [], "dependents": [], "cross_file_refs": []
            })
        });
        let mut value = serde_json::json!({
            "name": symbol.symbol_name,
            "qualified_name": symbol.qualified_name,
            "type": symbol.node_type,
            "byte_range": symbol.byte_range,
            "complexity": symbol.complexity,
            "dependencies": relation["dependencies"],
            "dependents": relation["dependents"],
            "cross_file_refs": relation["cross_file_refs"]
        });
        if include_source {
            value["source"] = Value::String(content[start..end].chars().take(500).collect());
        }
        total_chars += value.to_string().len();
        shown.push(value);
    }
    let function_count = shown
        .iter()
        .filter(|symbol| symbol["type"] == "function")
        .count();
    let class_count = shown
        .iter()
        .filter(|symbol| symbol["type"] == "class")
        .count();
    let symbol_count = catalog_total.unwrap_or(total);
    Ok(serde_json::json!({
        "file_path": file_path,
        "language": file_path.extension().and_then(|ext| ext.to_str()).unwrap_or("text"),
        "line_count": content.lines().count(),
        "symbol_count": symbol_count,
        "symbols_shown": shown.len(),
        "symbols_truncated": shown.len() < symbol_count || catalog_truncated,
        "catalog_truncated": catalog_truncated,
        "symbols": shown,
        "module_role": if class_count > function_count { "Class definitions" } else { "Function module" },
        "symbol_index_miss": symbol_index_miss,
        "source_freshness": "live",
        "pdg_status": pdg_status,
        "retrieval": {
            "tfidf_status": "not_used_exact",
            "neural_status": "not_used_exact",
            "partial": matches!(pdg_status, "partial" | "stale"),
            "max_latency_ms": budget.max_latency_ms,
            "allow_partial": budget.allow_partial
        }
    }))
}

async fn resident_file_relations(
    registry: &Arc<ProjectRegistry>,
    live: &LiveProject,
    symbols: &[CatalogSymbol],
    source_stale: bool,
    budget: WorkBudget,
    started: Instant,
) -> (HashMap<String, Value>, &'static str) {
    if source_stale {
        return (HashMap::new(), "stale");
    }

    let Some(handle) = registry.try_get_loaded(live.root()).await else {
        return (HashMap::new(), "not_loaded");
    };
    let guard = handle.read().await;
    let Some(pdg) = guard.pdg() else {
        return (HashMap::new(), "not_loaded");
    };
    let mut result = HashMap::new();
    for symbol in symbols {
        if budget.elapsed(started) {
            return (result, "partial");
        }
        let Some(node_id) = pdg
            .find_by_symbol(&symbol.node_id)
            .or_else(|| pdg.find_by_symbol(&symbol.symbol_name))
        else {
            continue;
        };
        let dependencies: Vec<Value> = pdg.neighbors(node_id).iter().filter_map(|&id| {
            let node = pdg.get_node(id)?;
            let file = live.file(&node.file_path).ok()?;
            Some(serde_json::json!({ "name": node.name, "type": node_type_str(&node.node_type), "file": file }))
        }).take(50).collect();
        let dependents: Vec<Value> = get_direct_callers(pdg, node_id).iter().filter_map(|&id| {
            let node = pdg.get_node(id)?;
            let file = live.file(&node.file_path).ok()?;
            Some(serde_json::json!({ "name": node.name, "type": node_type_str(&node.node_type), "file": file }))
        }).take(50).collect();
        let cross_file_refs: Vec<Value> = pdg.neighbors(node_id).iter().filter_map(|&id| {
            let node = pdg.get_node(id)?;
            let file = live.file(&node.file_path).ok()?;
            if file != symbol.file_path { Some(serde_json::json!({ "symbol": node.name, "file": file, "relationship": "dependency" })) } else { None }
        }).take(50).collect();
        result.insert(
            symbol.node_id.clone(),
            serde_json::json!({
                "dependencies": dependencies,
                "dependents": dependents,
                "cross_file_refs": cross_file_refs,
            }),
        );
    }
    (result, "fresh")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_file_summary_requires_indexed_project() {
        let dir = tempdir().unwrap();
        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({ "file_path": "/some/file.rs" });
        assert!(FileSummaryHandler.execute(&registry, args).await.is_err());
    }
}
