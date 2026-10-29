use super::helpers::{extract_bool, extract_string};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for LeIndex [index
///
/// Indexes a project by parsing all source files and building the search index.
#[derive(Clone)]
pub struct IndexHandler;

impl IndexHandler {
    /// Returns the name of this MCP tool (MCP-compliant: ASCII letters, digits, underscore, hyphen, dot only)
    pub fn name(&self) -> &str {
        "leindex_index"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Index]"
    }

    /// Returns the description of this RPC method
    pub fn description(&self) -> &str {
        "Starts a registry-owned project index job, or polls one with status_only=true. \
wait=true blocks for completion. Core PDG and TF-IDF results publish first, then the \
configured neural worker is actively evaluated for hybrid rows."
    }

    /// Returns the JSON schema for the arguments of this RPC method
    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Absolute path to the project directory to index"
                },
                "force_reindex": {
                    "type": "boolean",
                    "description": "If true, re-index even if already indexed. Also accepts '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "wait": {
                    "type": "boolean",
                    "description": "Wait for completion instead of a pollable snapshot",
                    "default": false
                },
                "status_only": {
                    "type": "boolean",
                    "description": "Poll without starting work"
                }
            },
            "required": ["project_path"]
        })
    }

    /// Executes the RPC method
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let project_path = extract_string(&args, "project_path")?;
        let force_reindex = extract_bool(&args, "force_reindex", false);
        let wait = extract_bool(&args, "wait", false);

        // Read-only poll: expose the job snapshot — including a job's
        // terminal Complete/Failed state — without touching the job table.
        // Without this, a caller polling by invoking the tool again could
        // never observe a terminal snapshot: reaching for a terminal job
        // replaces it with a fresh Running one, so fast jobs (the freshness
        // check turns re-runs into no-ops) restart on every poll.
        if extract_bool(&args, "status_only", false) {
            let snapshot = registry.get_index_job_snapshot(Some(&project_path)).await?;
            return match snapshot {
                Some(snapshot) => serde_json::to_value(snapshot).map_err(|e| {
                    JsonRpcError::internal_error(format!("Serialization error: {}", e))
                }),
                None => Ok(serde_json::json!({
                    "status": "none",
                    "note": "No index job has been started for this project in this process"
                })),
            };
        }

        let snapshot = registry
            .start_index_job(Some(&project_path), force_reindex, wait)
            .await?;
        let mut value = serde_json::to_value(snapshot)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

        // A failed job should still carry the last-known-good persisted state
        // (file count, generation, failure phase) so the caller gets partial
        // data instead of a bare failure.
        if value.get("status").and_then(Value::as_str) == Some("failed") {
            if let Some(health) = crate::cli::index_freshness::load_health(
                &crate::cli::leindex::resolve_existing_storage_path(std::path::Path::new(
                    &project_path,
                ))
                .unwrap_or_else(|| std::path::PathBuf::from(&project_path).join(".leindex")),
            ) {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "last_known_state".to_string(),
                        serde_json::json!({
                            "generation": health.generation,
                            "status": serde_json::to_value(health.status)
                                .unwrap_or(Value::Null),
                            "phase": serde_json::to_value(health.phase).unwrap_or(Value::Null),
                            "indexed_file_count": health.indexed_file_count,
                            "last_failure_phase": serde_json::to_value(health.last_failure_phase)
                                .unwrap_or(Value::Null),
                        }),
                    );
                }
            }
        }
        Ok(value)
    }
}
