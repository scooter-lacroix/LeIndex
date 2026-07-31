use super::helpers::{
    extract_bool, extract_usize, get_direct_callers, node_type_str, wrap_live_with_meta,
    wrap_live_with_meta_dirty,
};
use super::protocol::JsonRpcError;
use super::request_meta::{WorkBudget, record_git_ms, record_pdg_ms};
use crate::cli::git::{self, GitStatus};
use crate::cli::live_project::LiveProject;
use crate::cli::registry::ProjectRegistry;
use crate::graph::pdg::{NodeId, ProgramDependenceGraph, TraversalConfig};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Handler for live Git status with resident-PDG enrichment.
#[derive(Clone)]
pub struct GitStatusHandler;

#[allow(missing_docs)]
impl GitStatusHandler {
    pub fn name(&self) -> &str {
        "leindex.git-status"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Git Status]"
    }

    pub fn description(&self) -> &str {
        "Show live Git working tree status, enriched from an already-resident PDG when available."
    }

    pub fn argument_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_path": { "type": "string", "description": "Project directory; omit to use the configured project" },
                "max_latency_ms": { "type": "integer", "default": 150 },
                "allow_partial": { "type": "boolean", "default": true },
                "enrich_pdg": { "type": "boolean", "default": true, "description": "Accepted for compatibility; resident PDG enrichment remains enabled." },
                "scope": { "type": "string", "description": "Optional project-relative scope for advisory stage candidates" }
            },
            "required": []
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let raw_project = match args.get("project_path").and_then(Value::as_str) {
            Some(path) => PathBuf::from(path),
            None => registry.default_project_path().await?,
        };
        let live = LiveProject::resolve(&raw_project.to_string_lossy()).map_err(|error| {
            JsonRpcError::invalid_params(format!("Cannot resolve project path: {error}"))
        })?;
        let root = live.root().to_path_buf();
        let scope = resolve_scope(&args, &root)?;
        let budget = WorkBudget {
            max_latency_ms: extract_usize(&args, "max_latency_ms", 150)? as u64,
            allow_partial: extract_bool(&args, "allow_partial", true),
        };

        let git_started = Instant::now();
        let git_root = root.clone();
        let status = tokio::task::spawn_blocking(move || git::status(&git_root))
            .await
            .map_err(|error| {
                JsonRpcError::internal_error(format!("git status task failed: {error}"))
            })?;
        record_git_ms(git_started.elapsed().as_millis().min(u64::MAX as u128) as u64);

        let status = match status {
            Ok(status) => status,
            Err(git::GitStatusError::NotRepository) => {
                return Ok(wrap_live_with_meta(
                    json!({
                        "is_git_repo": false,
                        "message": "Not a git repository",
                        "pdg_status": "not_loaded",
                    }),
                    &root,
                ));
            }
            Err(error) => {
                return Err(JsonRpcError::internal_error(format!(
                    "git status failed: {error}"
                )));
            }
        };

        let live_dirty = status.modified.len()
            + status.staged.len()
            + status.untracked.len()
            + status.deleted.len();
        let mut result = wrap_live_with_meta_dirty(
            base_result(&status, &root, scope.as_deref()),
            &root,
            live_dirty,
        );
        let started = Instant::now();

        // This lookup is deliberately after the complete live Git response is built.
        // It neither creates nor loads an index generation.
        let Some(handle) = registry.try_get_loaded(&root).await else {
            return Ok(result);
        };
        let guard = handle.read().await;
        let Some(pdg) = guard.pdg() else {
            return Ok(result);
        };
        if let Some(health) = crate::cli::index_freshness::load_health(guard.storage_path()) {
            if matches!(
                health.status,
                crate::cli::leindex::ComponentStatus::Stale
                    | crate::cli::leindex::ComponentStatus::Partial
                    | crate::cli::leindex::ComponentStatus::Failed
            ) {
                result["pdg_status"] = Value::String(
                    match health.status {
                        crate::cli::leindex::ComponentStatus::Partial => "partial",
                        crate::cli::leindex::ComponentStatus::Failed => "failed",
                        _ => "stale",
                    }
                    .to_string(),
                );
                return Ok(result);
            }
        }

        let pdg_started = Instant::now();
        match enrich_pdg(pdg, &status, &root, budget, started) {
            Ok(enrichment) => {
                result["pdg_status"] = Value::String(enrichment.status.to_string());
                result["changed_symbols"] = Value::Array(enrichment.changed_symbols);
                result["impact_summary"] = json!({
                    "total_affected_symbols": enrichment.affected.len(),
                    "affected_files": enrichment.affected_files,
                    "pdg_enriched": true,
                });
            }
            Err(error) => {
                result["pdg_status"] = Value::String("failed".to_string());
                result["pdg_error"] = Value::String(error);
            }
        }
        record_pdg_ms(pdg_started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        Ok(result)
    }
}

fn base_result(status: &GitStatus, root: &Path, scope: Option<&Path>) -> Value {
    let safe_to_stage = tracked_paths(status)
        .into_iter()
        .filter(|path| safe_stage_candidate(path, status, root, scope))
        .map(|path| path_string(&path))
        .collect::<Vec<_>>();
    let categories = change_categories(status);
    json!({
        "is_git_repo": true,
        "branch": status.branch,
        "head_oid": status.head_oid,
        "summary": {
            "modified": status.modified.len(),
            "staged": status.staged.len(),
            "untracked": status.untracked.len(),
            "conflicted": status.conflicted.len(),
        },
        "change_categories": categories,
        "modified_files": paths(&status.modified),
        "staged_files": paths(&status.staged),
        "untracked_files": paths(&status.untracked),
        "conflicted_files": paths(&status.conflicted),
        "renames": status.renames.iter().map(|rename| json!({
            "from": path_string(&rename.from), "to": path_string(&rename.to)
        })).collect::<Vec<_>>(),
        "submodules": status.submodules.iter().map(|submodule| json!({
            "path": path_string(&submodule.path), "state": submodule.state
        })).collect::<Vec<_>>(),
        "safe_to_stage": safe_to_stage,
        "changed_symbols": [],
        "impact_summary": { "total_affected_symbols": 0, "affected_files": [], "pdg_enriched": false },
        "pdg_status": "not_loaded",
        "retrieval": { "tfidf_status": "not_used_live_status", "neural_status": "not_used_live_status" },
    })
}

fn change_categories(status: &GitStatus) -> Value {
    let mut tracked_source = 0usize;
    let mut untracked_notes = 0usize;
    let mut other = 0usize;
    let mut seen = HashSet::new();

    for path in status
        .modified
        .iter()
        .chain(status.staged.iter())
        .chain(status.deleted.iter())
    {
        if !seen.insert(path.clone()) {
            continue;
        }
        if is_source_path(path) {
            tracked_source += 1;
        } else {
            other += 1;
        }
    }
    for path in &status.untracked {
        if !seen.insert(path.clone()) {
            continue;
        }
        if is_note_path(path) {
            untracked_notes += 1;
        } else {
            other += 1;
        }
    }

    json!({
        "tracked_source": tracked_source,
        "untracked_notes": untracked_notes,
        "other": other,
        "submodules": status.submodules.len(),
    })
}

fn is_note_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == ".omx")
        || matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("md" | "txt" | "rst")
        )
}

fn is_source_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(
            "rs" | "py"
                | "js"
                | "ts"
                | "tsx"
                | "jsx"
                | "go"
                | "java"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "cs"
                | "rb"
                | "php"
                | "swift"
                | "kt"
                | "kts"
                | "lua"
                | "sh"
                | "bash"
                | "zsh"
                | "zig"
        )
    )
}

struct PdgEnrichment {
    status: &'static str,
    changed_symbols: Vec<Value>,
    affected: Vec<NodeId>,
    affected_files: Vec<String>,
}

fn enrich_pdg(
    pdg: &ProgramDependenceGraph,
    status: &GitStatus,
    root: &Path,
    budget: WorkBudget,
    started: Instant,
) -> Result<PdgEnrichment, String> {
    let mut roots = HashSet::new();
    let mut changed_symbols = Vec::new();
    let mut partial = false;

    for (path, state) in changed_paths(status) {
        let absolute = root.join(&path);
        let mut nodes = pdg.nodes_in_file(&absolute.to_string_lossy());
        if nodes.is_empty() {
            if let Ok(canonical) = absolute.canonicalize() {
                nodes = pdg.nodes_in_file(&canonical.to_string_lossy());
            }
        }
        let symbols = nodes
            .iter()
            .filter_map(|node_id| {
                let node = pdg.get_node(*node_id)?;
                roots.insert(*node_id);
                let callers = get_direct_callers(pdg, *node_id);
                Some(json!({
                    "name": node.name,
                    "type": node_type_str(&node.node_type),
                    "complexity": node.complexity,
                    "caller_count": callers.len(),
                    "callers": callers.iter().take(20).filter_map(|id| pdg.get_node(*id).map(|node| node.name.clone())).collect::<Vec<_>>(),
                }))
            })
            .collect::<Vec<_>>();
        changed_symbols.push(json!({
            "file": path_string(&path), "status": state, "symbols": symbols,
        }));
        if budget.elapsed(started) {
            partial = true;
            break;
        }
    }

    let affected = if partial || roots.is_empty() {
        Vec::new()
    } else {
        pdg.forward_impact_multi_source(
            &roots,
            &TraversalConfig {
                max_depth: Some(2),
                ..TraversalConfig::for_impact_analysis()
            },
        )
    };
    let mut affected_files: Vec<String> = affected
        .iter()
        .filter_map(|id| pdg.get_node(*id).map(|node| node.file_path.to_string()))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    affected_files.sort();
    Ok(PdgEnrichment {
        status: if partial { "partial" } else { "fresh" },
        changed_symbols,
        affected,
        affected_files,
    })
}

fn changed_paths(status: &GitStatus) -> Vec<(PathBuf, &'static str)> {
    let mut seen = HashSet::new();
    status
        .modified
        .iter()
        .map(|path| (path, "modified"))
        .chain(status.staged.iter().map(|path| (path, "staged")))
        // Include conflicted files in the set of changed paths for PDG enrichment.
        .chain(status.conflicted.iter().map(|path| (path, "conflicted")))
        .filter(|(path, _)| seen.insert((*path).clone()))
        .map(|(path, state)| (path.clone(), state))
        .collect()
}

fn tracked_paths(status: &GitStatus) -> Vec<PathBuf> {
    changed_paths(status)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

fn safe_stage_candidate(
    path: &Path,
    status: &GitStatus,
    root: &Path,
    scope: Option<&Path>,
) -> bool {
    let absolute = root.join(path);
    !status.deleted.contains(&path.to_path_buf())
        && !status.conflicted.contains(&path.to_path_buf())
        && !status
            .submodules
            .iter()
            .any(|submodule| submodule.path == path)
        && !path.starts_with(".leindex")
        && is_source_file(path)
        && scope.is_none_or(|scope| absolute.starts_with(scope))
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(
            "rs" | "py"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "go"
                | "java"
                | "kt"
                | "swift"
                | "rb"
                | "php"
                | "cs"
                | "scala"
                | "lua"
        )
    )
}

fn resolve_scope(args: &Value, root: &Path) -> Result<Option<PathBuf>, JsonRpcError> {
    let Some(raw) = args
        .get("scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty())
    else {
        return Ok(None);
    };
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    let scope = candidate.canonicalize().map_err(|error| {
        JsonRpcError::invalid_params(format!("Cannot resolve scope path '{raw}': {error}"))
    })?;
    if !scope.starts_with(root) {
        return Err(JsonRpcError::invalid_params(format!(
            "Scope '{raw}' is outside the project boundary '{}'",
            root.display()
        )));
    }
    Ok(Some(scope))
}

fn paths(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| path_string(path)).collect()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
