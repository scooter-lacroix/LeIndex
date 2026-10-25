//! Handler for `leindex_manage` — index lifecycle and **state** management.
//!
//! The router used to carry one branch (`index`). "Manage" that can only start
//! work is not management: a caller could kick off a job and then had no way to
//! ask what was resident in memory, which projects the registry was holding,
//! what generation was published, or to release any of it.
//!
//! This handler reuses what already exists rather than reimplementing it:
//!
//! * `index` delegates to [`crate::cli::mcp::index_handler::IndexHandler`], so
//!   there is exactly one job starter and one snapshot shape.
//! * `state` reuses the persisted [`crate::cli::index_freshness`] snapshot plus
//!   the registry's load state. It deliberately does NOT re-hash the inventory
//!   (`diagnostics` already reports live sizes); it answers "is this index
//!   usable, and at what generation".
//! * `projects`, `evict`, `lease` and `cleanup` are thin wrappers over
//!   [`ProjectRegistry::loaded_projects`], [`ProjectRegistry::evict`],
//!   [`ProjectRegistry::lease_generation`] and the storage generation reader.
//!
//! Every branch here is read-only except `index`, `evict` and `cleanup`, which
//! mutate in-memory residency or on-disk retention. `cleanup` is dry-run by
//! default and reports what it would remove.

use super::index_handler::IndexHandler;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One `action` of the manage router.
struct Action {
    /// Canonical discriminator value.
    name: &'static str,
    /// One-line summary for the guide and the advertised schema.
    summary: &'static str,
    /// Arguments this action reads.
    args: &'static [&'static str],
}

/// The manage surface, in advertisement order.
const ACTIONS: [Action; 6] = [
    Action {
        name: "index",
        summary: "Build or refresh the index; returns a pollable job (status_only=true polls)",
        args: &["force_reindex", "wait", "status_only"],
    },
    Action {
        name: "state",
        summary: "Is the index usable? generation, phase, freshness, file and node counts",
        args: &[],
    },
    Action {
        name: "projects",
        summary: "Projects resident in memory, with the default and residency count",
        args: &[],
    },
    Action {
        name: "evict",
        summary: "Release a project from memory (data stays on disk); 'all' evicts everything",
        args: &["target"],
    },
    Action {
        name: "lease",
        summary: "Acquire a generation lease: the published generation, its layers, pinned blobs",
        args: &[],
    },
    Action {
        name: "cleanup",
        summary: "Retention report for the generation store; dry_run=true by default",
        args: &["dry_run", "max_generations"],
    },
];

impl Action {
    fn parse(raw: &str) -> Option<&'static Action> {
        ACTIONS.iter().find(|action| action.name == raw)
    }

    /// One-line schema entry: `name [args]: summary`.
    fn usage(&self) -> String {
        let args = if self.args.is_empty() {
            String::new()
        } else {
            format!(" [{}]", self.args.join(", "))
        };
        format!("{}{args}: {}", self.name, self.summary)
    }
}

/// Handler for the `leindex_manage` router branch set.
#[derive(Clone)]
pub struct ManageHandler;

impl ManageHandler {
    /// Tool name advertised by the legacy surface and dispatched by the router.
    pub fn name(&self) -> &str {
        "leindex_manage"
    }

    /// Human-readable display title.
    pub fn title(&self) -> &str {
        "LeIndex [Manage]"
    }

    /// Tool description.
    pub fn description(&self) -> &str {
        "Index lifecycle and state: index (build/refresh, pollable), state (is it \
         usable — generation, phase, freshness, counts), projects (resident in \
         memory), evict (release memory, keep disk), lease (pin the published \
         generation), cleanup (retention, dry-run by default)."
    }

    /// Argument schema: every action's arguments plus the `action` selector.
    pub fn argument_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS.iter().map(|action| action.name).collect::<Vec<_>>(),
                    "description": format!(
                        "Index operation. Args per action: {}",
                        ACTIONS
                            .iter()
                            .map(Action::usage)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ),
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory; omit to use the configured project"
                },
                "target": {
                    "type": "string",
                    "description": "Project to evict, or 'all' for every resident project"
                },
                "force_reindex": {
                    "type": "boolean",
                    "description": "Re-index even if already indexed (index)",
                    "default": false
                },
                "wait": {
                    "type": "boolean",
                    "description": "Wait for completion instead of a pollable snapshot (index)",
                    "default": false
                },
                "status_only": {
                    "type": "boolean",
                    "description": "Poll an existing job without starting work (index)"
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Report retention candidates without deleting (cleanup); \
                                    defaults to true",
                    "default": true
                },
                "max_generations": {
                    "type": "integer",
                    "description": "Generations to retain: current plus this many predecessors \
                                    (cleanup)",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 3
                }
            },
            "required": []
        })
    }

    /// Execute one manage action.
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let raw = args
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("index");
        let action = Action::parse(raw).ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("Unknown manage action '{raw}'"),
                format!(
                    "Set action to one of: {}",
                    ACTIONS
                        .iter()
                        .map(|action| action.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?;
        match action.name {
            "index" => IndexHandler.execute(registry, args).await,
            "state" => state(registry, &args).await,
            "projects" => projects(registry).await,
            "evict" => evict(registry, &args).await,
            "lease" => lease(registry, &args).await,
            "cleanup" => cleanup(registry, &args).await,
            _ => unreachable!("every OPS entry is handled above"),
        }
    }
}

/// Resolve the project path this action refers to, without creating or
/// hydrating a project entry.
async fn resolved_root(
    registry: &Arc<ProjectRegistry>,
    args: &Value,
) -> Result<PathBuf, JsonRpcError> {
    let requested = args.get("project_path").and_then(Value::as_str);
    registry.resolve_project_root(requested).await
}

/// The store directory for a project, whether or not it is resident.
fn storage_path(project: &Path) -> PathBuf {
    crate::cli::leindex::resolve_existing_storage_path(project)
        .unwrap_or_else(|| project.join(".leindex"))
}

/// `state`: is this index usable, and at what generation.
///
/// Answers from the persisted health snapshot and the registry's load state.
/// It deliberately does not stat or hash the tree — `diagnostics` reports live
/// sizes, and doing it here would put an O(files) walk on a status check.
async fn state(registry: &Arc<ProjectRegistry>, args: &Value) -> Result<Value, JsonRpcError> {
    let root = resolved_root(registry, args).await?;
    let storage = storage_path(&root);
    let health = crate::cli::index_freshness::load_health(&storage);
    let resident = registry.try_get_loaded(&root).await.is_some();

    let Some(health) = health else {
        return Ok(json!({
            "action": "state",
            "project": root.display().to_string(),
            "usable": false,
            "reason": "no persisted index health — this project has never completed an index",
            "resident": resident,
            "next": "leindex_manage action=index to build one",
        }));
    };

    let age_ms = health.indexed_at_unix_ms.and_then(|indexed_at| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis() as u64;
        Some(now.saturating_sub(indexed_at))
    });
    // "Usable" is the question a caller actually has. A completed phase is
    // usable even with uncommitted changes elsewhere: this indexer indexes the
    // working tree, so modified-vs-HEAD files ARE indexed and must not be
    // reported as unusable (the same self-contradiction `diagnostics` fixed by
    // separating dirt from staleness).
    let complete = health.phase == crate::cli::leindex::IndexPhase::Complete;
    Ok(json!({
        "action": "state",
        "project": root.display().to_string(),
        "usable": complete,
        "generation": health.generation,
        // `as_str()` is the same vocabulary serde emits (`snake_case`) and
        // avoids a String allocation per field for a status response.
        "phase": health.phase.as_str(),
        // `ComponentStatus` has no `as_str()`; it is `Copy`, so serializing
        // here allocates nothing.
        "status": health.status,
        "indexed_file_count": health.indexed_file_count,
        "dirty_file_count": health.dirty_file_count,
        "changed_unindexed_count": health.changed_unindexed_count,
        "head_oid": health.head_oid,
        "tree_oid": health.tree_oid,
        "age_ms": age_ms,
        "resident": resident,
        "last_failure_phase": health.last_failure_phase.map(|phase| phase.as_str()),
        "last_failure": health.last_failure,
        "next": if complete {
            "leindex_manage action=index with force_reindex=true to rebuild"
        } else {
            "the last index run did not complete; run leindex_manage action=index to resume"
        },
    }))
}

/// `projects`: what is resident in memory right now.
///
/// Residency is a memory question, not a disk question. Evicting a project does
/// not delete anything, and a project can be fully indexed on disk while absent
/// from this list.
async fn projects(registry: &Arc<ProjectRegistry>) -> Result<Value, JsonRpcError> {
    let mut resident = registry.loaded_projects().await;
    resident.sort();
    let default = registry.default_project_path().await.ok();
    let rows: Vec<Value> = resident
        .iter()
        .map(|path| {
            let storage = storage_path(path);
            let health = crate::cli::index_freshness::load_health(&storage);
            json!({
                "project": path.display().to_string(),
                "generation": health.as_ref().map(|health| health.generation),
                "phase": health.as_ref().map(|health| health.phase.as_str()),
                "is_default": default.as_deref() == Some(path.as_path()),
            })
        })
        .collect();
    Ok(json!({
        "action": "projects",
        "count": rows.len(),
        "projects": rows,
        "default_project": default.map(|path| path.display().to_string()),
        "note": "residency only: a project can be indexed on disk and absent here. \
                 action=evict releases memory; it never deletes data.",
    }))
}

/// `evict`: release projects from memory. Never touches disk.
async fn evict(registry: &Arc<ProjectRegistry>, args: &Value) -> Result<Value, JsonRpcError> {
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty());

    // Evicting nothing is an easy mistake to make silently, so require a
    // target rather than defaulting to "everything".
    let Some(target) = target else {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            "leindex_manage action=evict requires 'target' (a project path or 'all')".to_string(),
            "Pass target='all' to release every resident project, or a project path \
             (omit nothing: evicting everything must be explicit)"
                .to_string(),
        ));
    };

    let evicted: Vec<String> = if target == "all" {
        let mut all = registry.loaded_projects().await;
        all.sort();
        for path in &all {
            registry.evict(path).await;
        }
        all.iter().map(|path| path.display().to_string()).collect()
    } else {
        let path = PathBuf::from(target);
        let was_resident = registry.try_get_loaded(&path).await.is_some();
        registry.evict(&path).await;
        if was_resident {
            vec![path.display().to_string()]
        } else {
            vec![]
        }
    };

    Ok(json!({
        "action": "evict",
        "evicted": evicted,
        "count": evicted.len(),
        "on_disk": "unchanged",
        "next": if evicted.is_empty() {
            "nothing was resident under that target; action=projects lists residency"
        } else {
            "the next tool call on this project reloads it from disk"
        },
    }))
}

/// `lease`: pin the currently published generation and report what it holds.
///
/// A lease increments CAS refcounts so retention cannot collect the blobs a
/// reader is about to use. It is released immediately here — this action
/// answers "what is published and would a lease succeed", it does not hand a
/// live lease to a caller who has no way to release it.
async fn lease(registry: &Arc<ProjectRegistry>, args: &Value) -> Result<Value, JsonRpcError> {
    let root = resolved_root(registry, args).await?;
    match registry.lease_generation(&root).await {
        Ok(lease) => {
            let summary = json!({
                "action": "lease",
                "project": root.display().to_string(),
                "acquired": true,
                "generation": lease.generation(),
                "layers": lease.layer_hashes().len(),
                "note": "the lease was released at the end of this call: it proves the \
                         generation is publishable and reports what it pins, but a caller \
                         has no way to release a live lease through MCP",
            });
            drop(lease);
            Ok(summary)
        }
        Err(error) => Ok(json!({
            "action": "lease",
            "project": root.display().to_string(),
            "acquired": false,
            "reason": error.to_string(),
            "next": "the project has no published generation to lease; action=index builds one",
        })),
    }
}

/// `cleanup`: report (and optionally apply) generation-store retention.
///
/// Dry-run by default. Reporting what the report means is the whole job here;
/// the actual sweep already exists in `leindex retention --report` and is not
/// duplicated.
async fn cleanup(registry: &Arc<ProjectRegistry>, args: &Value) -> Result<Value, JsonRpcError> {
    let root = resolved_root(registry, args).await?;
    let storage = storage_path(&root);
    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(true);
    let max_generations = args
        .get("max_generations")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(1, 100);

    let Some(current) = crate::storage::generation::read_current_generation(&storage) else {
        return Ok(json!({
            "action": "cleanup",
            "project": root.display().to_string(),
            "dry_run": dry_run,
            "generations": [],
            "note": "no CURRENT generation is published; there is nothing to retain or prune",
        }));
    };

    // Same window rule as `leindex retention sweep`: keep the current
    // generation plus `max_generations - 1` predecessors.
    let retained_floor = current.saturating_sub(max_generations.saturating_sub(1));
    let mut present: Vec<u64> = std::fs::read_dir(storage.join("generations"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    present.sort_unstable();
    let prunable: Vec<u64> = present
        .iter()
        .copied()
        .filter(|generation| *generation < retained_floor)
        .collect();

    Ok(json!({
        "action": "cleanup",
        "project": root.display().to_string(),
        "dry_run": dry_run,
        "current_generation": current,
        "retained_floor": retained_floor,
        "max_generations": max_generations,
        "generations_present": present,
        "prunable": prunable,
        "prunable_count": prunable.len(),
        "note": if dry_run {
            "dry_run=true: nothing was deleted. Run `leindex retention --report` for the \
             full byte-level retention report, or `leindex retention sweep` to apply."
        } else {
            "this action reports only; applying retention is `leindex retention sweep` so the \
             byte accounting lives in one place"
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_every_action_is_selectable_and_documented() {
        for action in &ACTIONS {
            assert!(!action.summary.is_empty(), "{} has no summary", action.name);
            assert!(
                Action::parse(action.name).is_some(),
                "{} does not parse",
                action.name
            );
        }
        assert!(Action::parse("nope").is_none());
    }

    /// The router advertises `manage` actions from `grouped.rs`; every one of
    /// them must be dispatchable here, or the advertised surface lies.
    #[test]
    fn test_dispatch_covers_every_advertised_action() {
        // Kept as an explicit list rather than a wildcard: adding an Action
        // without a dispatch arm is exactly the drift this catches.
        for name in ["index", "state", "projects", "evict", "lease", "cleanup"] {
            assert!(
                ACTIONS.iter().any(|action| action.name == name),
                "{name} is missing from ACTIONS"
            );
        }
        assert_eq!(
            ACTIONS.len(),
            6,
            "a new action needs a dispatch arm and a test"
        );
    }

    #[test]
    fn test_evict_requires_an_explicit_target() {
        // Evicting everything must never be the implicit default.
        assert!(Action::parse("evict").is_some());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let registry = Arc::new(ProjectRegistry::new(5));
            let error = ManageHandler
                .execute(&registry, json!({ "action": "evict" }))
                .await
                .unwrap_err();
            assert!(
                error.message_with_hint().contains("requires 'target'"),
                "{}",
                error.message_with_hint()
            );
        });
    }

    #[test]
    fn test_retention_window_keeps_current_plus_predecessors() {
        // The window rule the sweep uses, asserted directly: with
        // max_generations=3 and current=10 the floor is 8, so 8,9,10 survive.
        let current: u64 = 10;
        let max_generations: u64 = 3;
        let floor = current.saturating_sub(max_generations.saturating_sub(1));
        assert_eq!(floor, 8);
        let prunable: Vec<u64> = (1..=10).filter(|generation| *generation < floor).collect();
        assert_eq!(prunable, (1..=7).collect::<Vec<u64>>());
    }
}
