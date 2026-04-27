use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::sync::Arc;

/// Handler for leindex_git_status — PDG-aware git status.
#[derive(Clone)]
pub struct GitStatusHandler;

#[allow(missing_docs)]
impl GitStatusHandler {
    pub fn name(&self) -> &str {
        "leindex_git_status"
    }

    pub fn description(&self) -> &str {
        "Show git working tree status enriched with PDG structural analysis. \
        Maps changed files to affected symbols and computes impact radius."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": { "type": "string" },
                "include_diff": { "type": "boolean", "default": false },
                "diff_context_lines": { "type": "integer", "default": 3 }
            }
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let include_diff = extract_bool(&args, "include_diff", false);
        let diff_context_lines = extract_usize(&args, "diff_context_lines", 3)?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let index = handle.lock().await;
        let project_root = index.project_path().to_path_buf();

        if !project_root.join(".git").exists() {
            return Ok(wrap_with_meta(serde_json::json!({ "is_git_repo": false }), &index));
        }

        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&project_root)
            .output()
            .map_err(|e| JsonRpcError::internal_error(format!("Git failed: {}", e)))?;

        let status_text = String::from_utf8_lossy(&output.stdout);
        let mut modified = Vec::new();
        for line in status_text.lines() {
            if line.len() > 3 {
                modified.push(line[3..].trim().to_string());
            }
        }

        let branch = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&project_root)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let mut diff = None;
        if include_diff {
            let d_output = std::process::Command::new("git")
                .args(["diff", &format!("-U{}", diff_context_lines)])
                .current_dir(&project_root)
                .output()
                .ok();
            if let Some(o) = d_output {
                diff = Some(String::from_utf8_lossy(&o.stdout).to_string());
            }
        }

        Ok(wrap_with_meta(serde_json::json!({
            "is_git_repo": true,
            "branch": branch,
            "modified_files": modified,
            "diff": diff
        }), &index))
    }
}
