use super::edit_batch;
use super::edit_cache::{EditCacheEntry, GLOBAL_EDIT_CACHE};
use super::edit_risk;
use super::helpers::{
    apply_changes_in_memory, extract_string, make_diff, parse_edit_changes,
    validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::ResolvedEditChange;
use crate::graph::pdg::ProgramDependenceGraph;
use crate::validation::validation_to_json;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic nonce for preview tokens. Timestamps + pid alone can repeat on
/// fast multi-threaded calls (same millisecond, same process), and a reused
/// token would let an apply replay the WRONG cached preview.
static PREVIEW_TOKEN_NONCE: AtomicU64 = AtomicU64::new(0);

/// Handler for LeIndex [edit_preview — dry-run for code changes.
#[derive(Clone)]
pub struct EditPreviewHandler;

/// Per-file summary (`{path, additions, deletions}`) for preview/apply payloads.
pub(crate) fn file_summary(file: &edit_batch::BatchFileEdit) -> Value {
    let diff = make_diff(&file.original, &file.modified, &file.display_path);
    serde_json::json!({
        "path": file.display_path,
        "additions": diff.additions,
        "deletions": diff.deletions,
    })
}

/// Build the per-file edit plan (batch or single) and the primary display path.
///
/// All in memory: nothing is written, and the plan is what the diff, the
/// impact analysis and the cached preview are all derived from.
async fn build_preview_plan(
    changes_val: &Value,
    batch_mode: bool,
    top_level_path: Option<&str>,
    project_root: &Path,
) -> Result<(edit_batch::BatchEditPlan, String), JsonRpcError> {
    if batch_mode {
        let plan = edit_batch::resolve_batch(changes_val, top_level_path, project_root).await?;
        let primary = plan
            .files
            .first()
            .map(|f| f.display_path.clone())
            .unwrap_or_default();
        return Ok((plan, primary));
    }
    let file_path = top_level_path.unwrap_or_default();
    let abs_file_path = validate_file_within_project(file_path, project_root)?;
    let original = tokio::fs::read_to_string(&abs_file_path)
        .await
        .map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{file_path}': {e}"))
        })?;
    let changes = parse_edit_changes(changes_val, Some(&original))?;
    let modified = apply_changes_in_memory(&original, &changes)?;
    let plan = edit_batch::BatchEditPlan {
        files: vec![edit_batch::BatchFileEdit {
            canonical_path: abs_file_path,
            display_path: file_path.to_string(),
            original,
            modified,
            changes,
        }],
    };
    Ok((plan, file_path.to_string()))
}

/// Diffs, PDG impact and real-signal risk for every file in the plan.
struct PreviewAnalysis {
    files_json: Vec<Value>,
    diffs_json: Vec<Value>,
    diff_text: String,
    affected_files: Vec<String>,
    affected_nodes: Vec<String>,
    breaking_changes: Vec<String>,
    risk_level: String,
    risk_findings: Vec<String>,
}

/// Walk the plan once, accumulating the rendered diff and the impact/risk
/// findings per file.
///
/// Low risk contributes nothing. When a batch touches several files, the
/// response reports the HIGHEST risk (a later low-risk file must not downgrade
/// an earlier high-risk finding), which `pick_higher_risk` implements.
fn analyze_plan(
    plan: &edit_batch::BatchEditPlan,
    pdg_snapshot: Option<&ProgramDependenceGraph>,
) -> PreviewAnalysis {
    let mut analysis = PreviewAnalysis {
        files_json: Vec::new(),
        diffs_json: Vec::new(),
        diff_text: String::new(),
        affected_files: Vec::new(),
        affected_nodes: Vec::new(),
        breaking_changes: Vec::new(),
        risk_level: String::new(),
        risk_findings: Vec::new(),
    };
    for file in &plan.files {
        let diff = make_diff(&file.original, &file.modified, &file.display_path);
        analysis.files_json.push(file_summary(file));
        analysis
            .diff_text
            .push_str(&crate::cli::mcp::output::render_unified_diff(&diff, false));
        analysis.diffs_json.push(serde_json::json!({
            "file": file.display_path,
            "diff": diff.to_json(),
        }));

        let edit_ranges: Vec<(usize, usize)> = file
            .changes
            .iter()
            .filter_map(|c| match c {
                crate::edit::EditChange::ReplaceText { start, end, .. } => Some((*start, *end)),
                _ => None,
            })
            .collect();

        // Per-file PDG impact (rename callers + edited-region overlap).
        if let Some(pdg) = pdg_snapshot {
            let (nodes, files, breaks) =
                collect_pdg_impact(pdg, &file.changes, &file.canonical_path, &file.display_path);
            analysis.affected_nodes.extend(nodes);
            analysis.affected_files.extend(files);
            analysis.breaking_changes.extend(breaks);
        } else {
            analysis
                .affected_files
                .push(file.canonical_path.to_string_lossy().to_string());
        }

        let signals = edit_risk::compute_signals(
            pdg_snapshot,
            &file.canonical_path,
            &file.original,
            &edit_ranges,
        );
        if let Some(finding) = edit_risk::assess(&signals) {
            analysis.risk_level = pick_higher_risk(&analysis.risk_level, finding.level).to_string();
            analysis.risk_findings.push(format!(
                "{}: {} — {}",
                file.display_path, finding.level, finding.reason
            ));
        }
    }
    // Deduplicate affected lists while keeping first-seen order.
    dedup_keep_order(&mut analysis.affected_files);
    dedup_keep_order(&mut analysis.affected_nodes);
    dedup_keep_order(&mut analysis.breaking_changes);
    analysis
}

#[allow(missing_docs)]
impl EditPreviewHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_preview"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Edit Preview]"
    }

    pub fn description(&self) -> &str {
        "Preview a code edit: returns a preview_token on the first line plus a unified \
diff (changed lines in full, ±1 context). THE ADVERTISED WORKFLOW: preview, then \
apply with preview_token + project_path ONLY — do not resend the edit text. \
Accepts file_path + old_text/new_text, or a batch changes[] whose entries carry \
their own file_path (atomic across files, one token for the whole batch)."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or project-relative path to the file to edit. Relative paths resolve against the project root. Optional when every changes[] entry carries its own file_path."
                },
                "old_text": {
                    "type": "string",
                    "description": "Simple mode: text to find and replace (exact match)"
                },
                "new_text": {
                    "type": "string",
                    "description": "Simple mode: replacement text"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (omit to use current project). Indexing is NOT automatic: an unindexed project answers with a 'project not indexed' error until you run the index action."
                },
                "changes": {
                    "type": "array",
                    "description": "Advanced mode: list of changes. Single file: each has 'type' (replace_text/rename_symbol). Batch: each replace_text carries its own 'file_path' (applied atomically across files; one preview_token for the whole batch).",
                    "items": { "type": "object" }
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
        // Batch mode is detected from the changes[] entries; the top-level
        // file_path is optional there.
        let changes_val = resolve_changes_from_args(&args);
        let changes_empty = changes_val
            .as_array()
            .map(|arr| arr.is_empty())
            .unwrap_or(true);
        if changes_empty {
            return Err(JsonRpcError::invalid_params(
                "Provide either 'changes' or 'old_text'/'new_text' to preview.",
            ));
        }
        let batch_mode = edit_batch::is_batch(&changes_val);
        let top_level_path = args.get("file_path").and_then(|v| v.as_str());
        if !batch_mode {
            // Single-file form: file_path stays required (root cause of the
            // "which file?" ambiguity lives in the request shape, not here).
            extract_string(&args, "file_path")?;
        }

        let project_path_arg = args.get("project_path").and_then(|v| v.as_str());
        let handle = registry.get_or_create(project_path_arg).await?;

        // 1. Resolve path(s); the PDG load is best-effort. A preview of a
        // plain text edit must never be blocked by a degraded index — the
        // diff is exact and self-contained, and validation/impact degrade
        // to file-level when no PDG can be loaded.
        let (project_root, storage_path, pdg_available, pdg_snapshot) = {
            let mut guard = handle.write().await;
            if let Err(error) = guard.ensure_pdg_loaded_graph_only() {
                tracing::warn!(
                    project = %guard.project_path().display(),
                    "PDG unavailable for edit preview; continuing with file-level validation only: {error}"
                );
            }
            let pdg_available = guard.pdg().is_some();
            let pdg_snapshot = guard.pdg().cloned();
            (
                guard.project_path().to_path_buf(),
                guard.storage_path().to_path_buf(),
                pdg_available,
                pdg_snapshot,
            )
        };

        let nonce = PREVIEW_TOKEN_NONCE.fetch_add(1, Ordering::Relaxed);
        let preview_token = build_preview_token(nonce);

        // 2. Build the per-file edit plan (batch or single) — all in-memory.
        let (plan, primary_display) =
            build_preview_plan(&changes_val, batch_mode, top_level_path, &project_root).await?;

        // 3. Cache under the primary file path (existing path-keyed flow)
        // plus the token alias so apply works with ONLY preview_token.
        let primary_idx = plan
            .files
            .iter()
            .position(|f| f.display_path == primary_display)
            .ok_or_else(|| JsonRpcError::internal_error("batch plan lost its primary file"))?;
        let primary_file = &plan.files[primary_idx];
        let cache_entry = if plan.files.len() == 1 {
            EditCacheEntry::single_file(
                primary_file.canonical_path.clone(),
                preview_token.clone(),
                primary_file.original.clone(),
                primary_file.modified.clone(),
                primary_file.changes.clone(),
            )
        } else {
            let mut hashes = std::collections::HashMap::new();
            for file in &plan.files {
                hashes.insert(
                    file.canonical_path.to_string_lossy().to_string(),
                    blake3::hash(file.original.as_bytes()).to_hex().to_string(),
                );
            }
            EditCacheEntry {
                file_path: primary_file.canonical_path.clone(),
                preview_token: preview_token.clone(),
                original_text: primary_file.original.clone(),
                modified_text: primary_file.modified.clone(),
                changes: plan.files.iter().flat_map(|f| f.changes.clone()).collect(),
                timestamp: chrono::Utc::now(),
                original_hashes: hashes,
            }
        };
        store_preview_in_cache(&storage_path, cache_entry).await?;

        // 4. Diffs, impact, risk.
        let PreviewAnalysis {
            files_json,
            diffs_json,
            diff_text,
            affected_files,
            affected_nodes,
            breaking_changes,
            risk_level,
            risk_findings,
        } = analyze_plan(&plan, pdg_snapshot.as_ref());

        let mut response = serde_json::json!({
            "preview_token": preview_token,
            "files": files_json,
            "diff_text": diff_text,
            "affected_symbols": affected_nodes,
            "affected_files": affected_files,
            "breaking_changes": breaking_changes,
            "change_count": plan.change_count(),
            "multi_file": plan.files.len() > 1,
        });
        if batch_mode {
            response["diffs"] = Value::Array(diffs_json);
        } else {
            response["diff"] = diffs_json[0]["diff"].clone();
        }
        if !risk_level.is_empty() {
            response["risk"] = serde_json::json!({
                "level": risk_level,
                "reason": risk_findings.join("; "),
            });
        }
        if !pdg_available {
            response["pdg_status"] = serde_json::json!("not_loaded");
            response["warning"] = serde_json::json!(
                "Index unavailable — this preview shows the exact diff only, without \
                PDG impact analysis or syntax validation. Reindex (LeIndex [Index] with \
                force_reindex=true) to restore full preview capabilities."
            );
        }

        // Single-file previews keep the validator report (raw JSON only;
        // the trimmed text surface does not carry it).
        if plan.files.len() == 1 {
            let validation_json = {
                let guard = handle.read().await;
                run_validation_result(
                    &guard,
                    &primary_file.canonical_path,
                    &primary_file.original,
                    &primary_file.modified,
                )
            };
            if let Some(validation) = validation_json
                && let Some(obj) = response.as_object_mut()
            {
                obj.insert("validation".to_string(), validation);
            }
        }

        let guard = handle.read().await;
        Ok(wrap_with_meta(response, &guard))
    }
}

/// Deduplicate a list, keeping first-seen order.
fn dedup_keep_order(items: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.clone()));
}

/// Numeric rank used to keep the HIGHEST risk across a multi-file batch (a
/// later low-risk file must not downgrade an earlier high-risk finding).
fn risk_rank(level: &str) -> u8 {
    match level {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// Keep the higher-risk level; ties keep the current (first-seen) level.
fn pick_higher_risk<'a>(current: &'a str, candidate: &'a str) -> &'a str {
    if risk_rank(candidate) > risk_rank(current) {
        candidate
    } else {
        current
    }
}

/// Mint a preview token: a hash of (now + pid) with a process-local monotonic
/// nonce suffix. The nonce is what guarantees uniqueness across rapid calls —
/// timestamps + pid alone can repeat within the same millisecond.
fn build_preview_token(nonce: u64) -> String {
    format!(
        "{}-{}",
        blake3::hash(
            format!("{}{}", chrono::Utc::now().to_rfc3339(), std::process::id()).as_bytes(),
        )
        .to_hex(),
        nonce
    )
}

/// Build the `changes` JSON value from explicit `changes` or the simple-mode
/// `old_text`/`new_text` (and `old_str`/`new_str`) aliases.
fn resolve_changes_from_args(args: &Value) -> Value {
    if let Some(changes) = args.get("changes").cloned() {
        return changes;
    }
    let old_text = args
        .get("old_text")
        .or_else(|| args.get("old_str"))
        .and_then(|v| v.as_str());
    let new_text = args
        .get("new_text")
        .or_else(|| args.get("new_str"))
        .and_then(|v| v.as_str());
    match (old_text, new_text) {
        (Some(old), Some(new)) => serde_json::json!([{
            "type": "replace_text",
            "old_text": old,
            "new_text": new
        }]),
        _ => Value::Array(vec![]),
    }
}

/// Store a prepared preview in the global cache. The primary file-path entry
/// must be durable for the advertised `preview_token` apply flow — a storage
/// or size-limit failure is surfaced, not swallowed.
async fn store_preview_in_cache(
    storage_path: &std::path::Path,
    cache_entry: EditCacheEntry,
) -> Result<(), JsonRpcError> {
    match GLOBAL_EDIT_CACHE
        .set_with_token_alias(storage_path, cache_entry)
        .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(JsonRpcError::invalid_params(e.to_string())),
        Err(e) => Err(e),
    }
}

/// Validate the resolved edit under a read guard, returning a JSON payload (or
/// `None` when no validator is available). A validator error degrades to a
/// `validation_warning` object rather than failing the whole preview.
fn run_validation_result(
    guard: &crate::cli::registry::ProjectReadGuard<'_>,
    abs_file_path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Option<Value> {
    let validator = guard.create_validator()?;
    let resolved = ResolvedEditChange::new(
        abs_file_path.to_path_buf(),
        original.to_string(),
        modified.to_string(),
    );
    match validator.validate_changes(&[resolved]) {
        Ok(result) => Some(validation_to_json(&result)),
        Err(e) => Some(serde_json::json!({
            "is_valid": Value::Null,
            "has_errors": false,
            "syntax_errors": [],
            "reference_issues": [],
            "semantic_drift": [],
            "impact_report": null,
            "validation_warning": format!("Validation check failed: {}", e)
        })),
    }
}

/// Collect PDG impact: rename callers/dependents plus symbols overlapping the
/// edited byte regions. `files` always seeds with the edited file.
fn collect_pdg_impact(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    abs_file_path: &std::path::Path,
    file_path: &str,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut files: Vec<String> = vec![abs_file_path.to_string_lossy().to_string()];

    let (nodes, breaks) = collect_rename_impact(pdg, changes, abs_file_path, &mut files);

    // For text replacements, find symbols in the edited file whose byte ranges
    // overlap with the changed region(s). Only runs when no rename impact was
    // found — avoids over-reporting symbols unrelated to the rename.
    let nodes = if nodes.is_empty() {
        collect_text_overlap_symbols(pdg, changes, file_path, abs_file_path)
    } else {
        nodes
    };

    (nodes, files, breaks)
}

/// For each rename change, record forward-impact symbol/file names and flag
/// any backward (caller) impact as a potential breaking change.
fn collect_rename_impact(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    abs_file_path: &std::path::Path,
    files: &mut Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let mut nodes: Vec<String> = Vec::new();
    let mut breaks: Vec<String> = Vec::new();
    for change in changes {
        let crate::edit::EditChange::RenameSymbol {
            old_name,
            new_name: _,
        } = change
        else {
            continue;
        };
        // Try name-based lookup for PDG impact analysis
        let found_id = pdg
            .find_by_symbol(old_name)
            .or_else(|| pdg.find_by_name(old_name))
            .or_else(|| pdg.find_by_name_in_file(old_name, Some(&abs_file_path.to_string_lossy())));
        let Some(node_id) = found_id else {
            continue;
        };
        let forward = pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        );
        for dep_id in &forward {
            if let Some(dn) = pdg.get_node(*dep_id) {
                nodes.push(dn.name.clone());
                files.push(dn.file_path.to_string());
            }
        }
        let backward = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        );
        if !backward.is_empty() {
            breaks.push(format!(
                "Renaming '{}' may break {} caller(s)",
                old_name,
                backward.len()
            ));
        }
    }
    (nodes, breaks)
}

/// Find symbols in the edited file whose byte range overlaps any ReplaceText
/// edit region. Tries the user-provided path (often relative) then the absolute.
fn collect_text_overlap_symbols(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    changes: &[crate::edit::EditChange],
    file_path: &str,
    abs_file_path: &std::path::Path,
) -> Vec<String> {
    let edit_ranges: Vec<(usize, usize)> = changes
        .iter()
        .filter_map(|c| match c {
            crate::edit::EditChange::ReplaceText { start, end, .. } => Some((*start, *end)),
            _ => None,
        })
        .collect();

    let file_nodes = pdg.nodes_in_file(file_path);
    let file_nodes = if file_nodes.is_empty() {
        pdg.nodes_in_file(&abs_file_path.to_string_lossy())
    } else {
        file_nodes
    };

    let mut nodes = Vec::new();
    for nid in file_nodes {
        let Some(n) = pdg.get_node(nid) else {
            continue;
        };
        // Only include symbols whose byte range overlaps at least one edit region.
        let overlaps = edit_ranges.iter().any(|&(es, ee)| {
            if es == ee {
                n.byte_range.0 <= es && es <= n.byte_range.1
            } else {
                // Two ranges [s1, e1) and [s2, e2) overlap when s1 < e2 && s2 < e1
                n.byte_range.0 < ee && es < n.byte_range.1
            }
        });
        if overlaps {
            nodes.push(n.name.clone());
        }
    }
    nodes
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_preview_tokens_differ_across_nonces() {
        // Regression (§3.4): tokens must be unique even when two previews run
        // within the same millisecond — the monotonic nonce suffix is the
        // guarantee, so two nonces never collide.
        let a = build_preview_token(0);
        let b = build_preview_token(1);
        assert_ne!(a, b, "nonce suffix must make tokens unique");
        assert!(a.ends_with("-0"), "token must carry the nonce: {a}");
        assert!(b.ends_with("-1"), "token must carry the nonce: {b}");
    }

    #[test]
    fn test_risk_rank_orders_levels() {
        assert!(risk_rank("high") > risk_rank("medium"));
        assert!(risk_rank("medium") > risk_rank("low"));
        assert!(risk_rank("low") > risk_rank("unknown"));
        assert_eq!(risk_rank("unknown"), 0);
    }

    #[test]
    fn test_pick_higher_risk_keeps_highest_and_first_tie() {
        // Regression (§3.6): a later low-risk file must not downgrade an
        // earlier high-risk finding; ties keep the first-seen level.
        assert_eq!(pick_higher_risk("", "high"), "high");
        assert_eq!(pick_higher_risk("high", "low"), "high");
        assert_eq!(pick_higher_risk("high", "medium"), "high");
        assert_eq!(pick_higher_risk("medium", "high"), "high");
        assert_eq!(pick_higher_risk("medium", "medium"), "medium");
        assert_eq!(pick_higher_risk("low", ""), "low");
    }
}
