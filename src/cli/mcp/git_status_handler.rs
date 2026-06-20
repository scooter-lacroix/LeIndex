use super::helpers::{
    extract_bool, extract_usize, get_direct_callers, node_type_str, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Handler for LeIndex [git_status — PDG-aware git status.
///
/// Unlike plain `git status`, maps changed files to affected PDG symbols
/// and computes forward impact (blast radius).
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
        "Show git working tree status enriched with PDG structural analysis. \
Maps changed files to affected symbols, their callers, and transitive forward impact. \
Turns a raw diff into a structural change summary with blast radius."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                },
                "include_diff": {
                    "type": "boolean",
                    "description": "Include unified diff content for modified files (default: false)",
                    "default": false
                },
                "diff_context_lines": {
                    "type": "integer",
                    "description": "Context lines for diff output (default: 3)",
                    "default": 3,
                    "minimum": 0,
                    "maximum": 20
                }
            },
            "required": []
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
        let mut guard = handle.write().await;

        let project_root = guard.project_path().to_path_buf();

        // Check if it's a git repo
        let git_dir = project_root.join(".git");
        if !git_dir.exists() {
            return Ok(wrap_with_meta(
                serde_json::json!({
                    "is_git_repo": false,
                    "message": "Not a git repository"
                }),
                &guard,
            ));
        }

        // Run git status --porcelain
        let status_output = std::process::Command::new("git")
            .args(["status", "--porcelain", "-uall"])
            .current_dir(&project_root)
            .output()
            .map_err(|e| {
                JsonRpcError::internal_error(format!("Failed to run git status: {}", e))
            })?;

        if !status_output.status.success() {
            return Err(JsonRpcError::internal_error(format!(
                "git status failed: {}",
                String::from_utf8_lossy(&status_output.stderr)
            )));
        }

        let status_text = String::from_utf8_lossy(&status_output.stdout);

        let pdg_error: Option<String> = {
            match guard.ensure_pdg_loaded() {
                Ok(()) => None,
                Err(e) => {
                    tracing::warn!(
                        "PDG load failed for git status enrichment in {}: {}",
                        project_root.display(),
                        e
                    );
                    Some(e.to_string())
                }
            }
        };
        let pdg = if pdg_error.is_none() {
            guard.pdg()
        } else {
            None
        };

        // Parse git status output
        let mut modified_files: Vec<String> = Vec::new();
        let mut staged_files: Vec<String> = Vec::new();
        let mut untracked_files: Vec<String> = Vec::new();

        for line in status_text.lines() {
            if line.len() < 4 {
                continue;
            }
            let status_code = &line[..2];
            let file = line[3..].trim().to_string();

            match status_code.trim() {
                "M" | "MM" | "AM" => modified_files.push(file),
                "A" | "A " => staged_files.push(file),
                "??" => untracked_files.push(file),
                "D" | "D " => staged_files.push(file),
                s if s.starts_with('M') => staged_files.push(file),
                s if s.ends_with('M') => modified_files.push(file),
                _ => modified_files.push(file),
            }
        }

        // Get current branch
        let branch_output = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&project_root)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        // PDG enrichment: map changed files to symbols
        let mut changed_symbols: Vec<Value> = Vec::new();
        let mut total_affected_symbols = 0usize;
        let mut affected_files_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        if let Some(pdg) = pdg {
            for file in modified_files.iter().chain(staged_files.iter()) {
                // Resolve to absolute path for PDG lookup
                let abs_path = if Path::new(file).is_absolute() {
                    PathBuf::from(file)
                } else {
                    project_root.join(file)
                };
                let abs_str = abs_path.to_string_lossy().to_string();

                let nodes = pdg.nodes_in_file(&abs_str);
                if nodes.is_empty() {
                    // Try with canonicalized path
                    let canon = abs_path.canonicalize().unwrap_or(abs_path);
                    let canon_str = canon.to_string_lossy().to_string();
                    let nodes = pdg.nodes_in_file(&canon_str);
                    if nodes.is_empty() {
                        changed_symbols.push(serde_json::json!({
                            "file": file,
                            "status": if modified_files.contains(file) { "modified" } else { "staged" },
                            "symbols": [],
                            "note": "No indexed symbols in this file"
                        }));
                        continue;
                    }
                }

                let mut file_symbols: Vec<Value> = Vec::new();
                for nid in &nodes {
                    if let Some(node) = pdg.get_node(*nid) {
                        let caller_ids = get_direct_callers(pdg, *nid);
                        let caller_count = caller_ids.len();
                        let callers: Vec<String> = caller_ids
                            .iter()
                            .take(20)
                            .filter_map(|&id| pdg.get_node(id).map(|n| n.name.clone()))
                            .collect();
                        let forward_impact = pdg.forward_impact(
                            *nid,
                            &crate::graph::pdg::TraversalConfig {
                                max_depth: Some(2),
                                ..crate::graph::pdg::TraversalConfig::for_impact_analysis()
                            },
                        );
                        total_affected_symbols += forward_impact.len();

                        // Track affected files
                        for &fid in &forward_impact {
                            if let Some(fnode) = pdg.get_node(fid) {
                                affected_files_set.insert(fnode.file_path.to_string());
                            }
                        }

                        file_symbols.push(serde_json::json!({
                            "name": node.name,
                            "type": node_type_str(&node.node_type),
                            "complexity": node.complexity,
                            "caller_count": caller_count,
                            "callers": callers,
                            "forward_impact_count": forward_impact.len(),
                        }));
                    }
                }

                let status = if modified_files.contains(file) {
                    "modified"
                } else {
                    "staged"
                };

                changed_symbols.push(serde_json::json!({
                    "file": file,
                    "status": status,
                    "symbols": file_symbols,
                }));
            }
        }

        // Optionally include diff
        let diff_content: Option<String> = if include_diff {
            std::process::Command::new("git")
                .args([
                    "diff",
                    &format!("--unified={}", diff_context_lines),
                    "--no-color",
                ])
                .current_dir(&project_root)
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        Some(String::from_utf8_lossy(&o.stdout).to_string())
                    } else {
                        None
                    }
                })
        } else {
            None
        };

        let affected_files: Vec<String> = affected_files_set.into_iter().collect();

        // Build pdg_enrichment status field for transparency about PDG availability
        let pdg_enrichment = if let Some(err) = &pdg_error {
            tracing::debug!("PDG load failed while enriching git status: {}", err);
            serde_json::json!({
                "available": false,
                "reason": "PDG load failed",
                "error": "PDG enrichment unavailable; run leindex index to refresh the project graph",
            })
        } else {
            serde_json::json!({
                "available": true,
            })
        };

        Ok(wrap_with_meta(
            serde_json::json!({
                "is_git_repo": true,
                "branch": branch_output,
                "summary": {
                    "modified": modified_files.len(),
                    "staged": staged_files.len(),
                    "untracked": untracked_files.len(),
                },
                "modified_files": modified_files,
                "staged_files": staged_files,
                "untracked_files": untracked_files,
                "changed_symbols": changed_symbols,
                "pdg_enrichment": pdg_enrichment,
                "impact_summary": {
                    "total_affected_symbols": total_affected_symbols,
                    "affected_files": affected_files,
                    "pdg_enriched": pdg.is_some(),
                },
                "diff": diff_content,
            }),
            &guard,
        ))
    }
}
