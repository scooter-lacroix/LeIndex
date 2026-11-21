use super::edit_batch;
use super::edit_cache::{EditCacheEntry, GLOBAL_EDIT_CACHE};
use super::edit_preview_handler::{EditPreviewHandler, file_summary};
use super::edit_validate::{self, ValidateRequest};
use super::helpers::{
    apply_changes_in_memory, extract_bool, parse_edit_changes, validate_file_within_project,
    wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::{ProjectHandle, ProjectRegistry};
use crate::edit::{ResolvedEditChange, atomic_write_with_expected_async};
use crate::validation::validation_to_json;
use serde_json::Value;
use std::sync::Arc;

type EditImpact = (Vec<String>, std::collections::HashSet<String>, Vec<String>);

/// What the caller asked `apply` to do. Token mode (preview_token, with or
/// without file_path) is the ADVERTISED default; fresh mode resends the text.
enum ApplyRequest {
    /// Apply the cached preview identified by this token.
    Token {
        token: String,
        /// Optional: cross-checks the cached preview's file when given.
        file_path: Option<String>,
    },
    /// Apply the edit text directly (path resolved later).
    Fresh { file_path: Option<String> },
}

fn apply_request_args(args: &Value) -> Result<ApplyRequest, JsonRpcError> {
    let token = args
        .get("preview_token")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let file_path = args
        .get("file_path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let has_text = args.get("old_text").is_some()
        || args.get("old_str").is_some()
        || args.get("new_text").is_some()
        || args.get("new_str").is_some()
        || args.get("changes").is_some();
    match (token, has_text) {
        (Some(_), true) => Err(JsonRpcError::invalid_params(
            "Send EITHER preview_token OR the edit text (old_text/new_text/changes) — \
             never both. The token already carries the previewed edit.",
        )),
        (Some(token), false) => Ok(ApplyRequest::Token { token, file_path }),
        (None, _) => Ok(ApplyRequest::Fresh { file_path }),
    }
}

async fn apply_atomic(
    path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Result<bool, JsonRpcError> {
    atomic_write_with_expected_async(
        path.to_path_buf(),
        modified.as_bytes().to_vec(),
        original.as_bytes().to_vec(),
    )
    .await
    .map_err(|error| {
        JsonRpcError::internal_error(format!("Failed to write '{}': {}", path.display(), error))
    })
}

async fn ensure_write_succeeded(
    success: bool,
    storage_path: &std::path::Path,
    canonical_path: &std::path::Path,
) -> Result<(), JsonRpcError> {
    if success {
        return Ok(());
    }
    GLOBAL_EDIT_CACHE.clear(storage_path, canonical_path).await;
    Err(JsonRpcError::invalid_params(
        "Edit rejected: file content changed on disk since preview was generated. \
        Please call LeIndex [Edit Preview] again (tool: leindex.edit-preview).",
    ))
}

/// Wrap a preview payload in the explicit dry-run envelope. Delegating raw made
/// the apply renderer read the preview-shaped payload (no `success` field) as
/// "Edit apply failed" with no detail — a dry-run that reports failure without a
/// reason is worse than none (N-09).
fn dry_run_envelope(preview: Value) -> Value {
    let mut envelope = serde_json::json!({
        "success": true,
        "dry_run": true,
        "changes_applied": 0,
        "message": "Dry run: no changes written. See `preview` for the diff and validation.",
    });
    if let (Some(obj), Some(preview_obj)) = (envelope.as_object_mut(), preview.as_object()) {
        for (key, value) in preview_obj {
            if key == "content" || key == "isError" {
                continue;
            }
            obj.insert(key.clone(), value.clone());
        }
    }
    envelope
}

/// Best-effort PDG load. Applying a plain text edit must never be blocked by a
/// degraded/unavailable index: the atomic write + expected-content guard is the
/// safety net, and impact/validation simply report as unavailable when no PDG
/// can be loaded.
async fn load_pdg_best_effort(handle: &ProjectHandle) -> bool {
    let mut guard = handle.write().await;
    match guard.ensure_pdg_loaded_graph_only() {
        Ok(()) => guard.pdg().is_some(),
        Err(error) => {
            tracing::warn!(
                project = %guard.project_path().display(),
                "PDG unavailable for edit-apply; continuing without PDG impact analysis: {error}"
            );
            false
        }
    }
}

/// Wrap a response in the live project metadata.
async fn wrap_with_current_meta(handle: &ProjectHandle, response: Value) -> Value {
    let guard = handle.read().await;
    wrap_with_meta(response, &guard)
}

/// Flag a response that was produced without a loaded PDG: the edit is durable,
/// but impact analysis and validation were unavailable for it.
fn annotate_degraded_pdg(response: &mut Value) {
    let Some(object) = response.as_object_mut() else {
        return;
    };
    object.insert("pdg_status".to_string(), serde_json::json!("not_loaded"));
    object.insert(
        "warning".to_string(),
        serde_json::json!(
            "Index unavailable — edit applied with file-level safety only \
            (no PDG impact analysis or validation). Reindex (LeIndex [Index] with \
            force_reindex=true) to restore full editing safeguards."
        ),
    );
}

/// Index maintenance AFTER the response value exists. One-shot CLI processes run
/// it inline (the process exits once the response is printed, so a spawned task
/// would be killed mid-write); the long-running MCP server/daemon spawns it so
/// the caller's latency is exactly the edit, never the reindex.
async fn refresh_index_after_edit(registry: &Arc<ProjectRegistry>, handle: &ProjectHandle) {
    use crate::cli::leindex::indexing::watcher_delta::NotHydratedError;

    // A not-hydrated resident can never take the incremental path — the
    // guard is a persistent condition, not a transient one — so escalate to
    // a full index once instead of leaving the project permanently stale.
    async fn refresh(guard: &mut crate::cli::leindex::LeIndex) -> Result<(), anyhow::Error> {
        if let Err(e) = guard.incremental_reindex_from_watcher() {
            if !e.is::<NotHydratedError>() {
                return Err(e);
            }
            tracing::warn!("Index refresh needs a full index ({e}); escalating");
            guard.index_project(true)?;
        }
        Ok(())
    }

    if registry.is_one_shot() {
        let mut guard = handle.write().await;
        if let Err(e) = refresh(&mut guard).await {
            tracing::warn!("Failed to refresh index after edit-apply: {}", e);
        }
        return;
    }
    let registry = Arc::clone(registry);
    let handle = Arc::clone(handle);
    tokio::spawn(async move {
        // Serialized behind the project write lock: concurrent
        // edit-applies queue their refreshes instead of racing.
        let mut guard = handle.write().await;
        let root = guard.project_path().to_path_buf();
        if let Err(e) = refresh(&mut guard).await {
            tracing::warn!(
                project = %root.display(),
                "Background refresh after edit-apply failed: {e}"
            );
        }
        drop(guard);
        registry.invalidate_stale_cache(&root).await;
        tracing::debug!(
            project = %root.display(),
            "Background refresh after edit-apply complete"
        );
    });
}

fn validate_edit(
    validator: Option<crate::validation::LogicValidator>,
    path: &std::path::Path,
    original: &str,
    modified: &str,
) -> Option<Value> {
    let validator = validator?;
    let change =
        ResolvedEditChange::new(path.to_path_buf(), original.to_owned(), modified.to_owned());
    match validator.validate_changes(&[change]) {
        Ok(result) => Some(validation_to_json(&result)),
        Err(error) => {
            tracing::warn!("Validation check failed: {}", error);
            None
        }
    }
}

fn edit_impact(
    pdg: Option<&crate::graph::pdg::ProgramDependenceGraph>,
    changes: &[crate::edit::EditChange],
    path: &std::path::Path,
) -> EditImpact {
    let mut nodes = Vec::new();
    let mut files = std::collections::HashSet::new();
    files.insert(path.to_string_lossy().to_string());
    let mut breaking = Vec::new();
    let Some(pdg) = pdg else {
        return (nodes, files, breaking);
    };
    for change in changes {
        let crate::edit::EditChange::RenameSymbol { old_name, .. } = change else {
            continue;
        };
        let node_id = pdg
            .find_by_symbol(old_name)
            .or_else(|| pdg.find_by_name(old_name))
            .or_else(|| pdg.find_by_name_in_file(old_name, Some(&path.to_string_lossy())));
        let Some(node_id) = node_id else {
            continue;
        };
        for dependency in pdg.forward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        ) {
            if let Some(node) = pdg.get_node(dependency) {
                nodes.push(node.name.clone());
                files.insert(node.file_path.to_string());
            }
        }
        let callers = pdg.backward_impact(
            node_id,
            &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
        );
        if !callers.is_empty() {
            breaking.push(format!(
                "Renaming '{}' may break {} caller(s)",
                old_name,
                callers.len()
            ));
        }
    }
    (nodes, files, breaking)
}

fn edit_region(original: &str, modified: &str) -> String {
    let modified_lines: Vec<&str> = modified.lines().collect();
    let original_lines: Vec<&str> = original.lines().collect();
    let shared_len = original_lines.len().min(modified_lines.len());
    let first_diff = original_lines
        .iter()
        .zip(modified_lines.iter())
        .position(|(old, new)| old != new)
        .unwrap_or(shared_len);
    let start = first_diff.saturating_sub(5);
    let end = (first_diff + 10).min(modified_lines.len());
    modified_lines[start..end]
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}: {}", start + index + 1, line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The §4.3 one-line echo: `Applied: path:L802-828 (+27/-0) in fn name`.
/// The enclosing-function suffix appears only when the PDG can name one.
fn applied_lines(
    files: &[edit_batch::BatchFileEdit],
    pdg: Option<&crate::graph::pdg::ProgramDependenceGraph>,
) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.modified != f.original)
        .map(|file| {
            let mut first_line = usize::MAX;
            let mut last_line = 0usize;
            for change in &file.changes {
                let crate::edit::EditChange::ReplaceText {
                    start, new_text, ..
                } = change
                else {
                    continue;
                };
                let prefix = &file.modified[..(*start).min(file.modified.len())];
                let line = prefix.matches('\n').count() + 1;
                let end = (prefix.len() + new_text.len()).min(file.modified.len());
                let end_line = file.modified[..end].matches('\n').count() + 1;
                first_line = first_line.min(line);
                last_line = last_line.max(end_line);
            }
            if first_line == usize::MAX {
                first_line = 1;
                last_line = 1;
            }
            let diff = crate::cli::mcp::output::compute_diff(
                &file.original,
                &file.modified,
                &file.display_path,
            );
            let enclosing = pdg.and_then(|graph| {
                let first_byte = file
                    .changes
                    .iter()
                    .filter_map(|c| match c {
                        crate::edit::EditChange::ReplaceText { start, .. } => Some(*start),
                        _ => None,
                    })
                    .min()?;
                graph
                    .nodes_in_file(&file.canonical_path.to_string_lossy())
                    .into_iter()
                    .filter_map(|nid| graph.get_node(nid))
                    .find(|node| node.byte_range.0 <= first_byte && first_byte < node.byte_range.1)
                    .map(|node| node.name.clone())
            });
            match enclosing {
                Some(name) => format!(
                    "Applied: {}:{}-{} (+{}/-{}) in fn {name}",
                    file.display_path, first_line, last_line, diff.additions, diff.deletions
                ),
                None => format!(
                    "Applied: {}:{}-{} (+{}/-{})",
                    file.display_path, first_line, last_line, diff.additions, diff.deletions
                ),
            }
        })
        .collect()
}

/// Handler for LeIndex [edit_apply — atomic code modifications.
#[derive(Clone)]
pub struct EditApplyHandler;

#[allow(missing_docs)]
impl EditApplyHandler {
    pub fn name(&self) -> &str {
        "leindex_edit_apply"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Edit Apply]"
    }

    pub fn description(&self) -> &str {
        "Apply a previewed edit with ONLY preview_token + project_path — the token \
IS the edit (advertised default; do not resend old_text/new_text). Without a \
preview: file_path + old_text + new_text, or a batch changes[] whose entries \
carry their own file_path (atomic across files). validate:\"syntax\" runs a \
tree-sitter parse of every edited file after the write."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute or project-relative path. Optional in TOKEN MODE (preview_token alone identifies the edit); required in FRESH MODE."
                },
                "old_text": {
                    "type": "string",
                    "description": "FRESH MODE: text to find and replace (exact match). Do NOT send alongside preview_token."
                },
                "old_str": {
                    "type": "string",
                    "description": "Alias for old_text (compatibility with edit_file)"
                },
                "new_text": {
                    "type": "string",
                    "description": "FRESH MODE: replacement text. Do NOT send alongside preview_token."
                },
                "new_str": {
                    "type": "string",
                    "description": "Alias for new_text (compatibility with edit_file)"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (omit to use current project). Indexing is NOT automatic: an unindexed project answers with a 'project not indexed' error until you run the index action."
                },
                "changes": {
                    "type": "array",
                    "description": "FRESH MODE batch: changes to apply, each optionally carrying its own 'file_path'. Entries across multiple files are applied atomically (all-or-nothing, rolled back on failure). Do NOT send alongside preview_token.",
                    "items": { "type": "object" }
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "If true, return preview without modifying files (default: false). \
        Also accepts compatibility strings: 'true'/'false', '1'/'0', 'yes'/'no'.",
                    "default": false
                },
                "validate": {
                    "type": "string",
                    "enum": ["syntax"],
                    "description": "\"syntax\": tree-sitter-parse every edited file after the write and report errors (line:col)."
                },
                "preview_token": {
                    "type": "string",
                    "description": "TOKEN MODE (advertised default): the preview_token from LeIndex [Edit Preview]. Send preview_token + project_path ONLY — the token replaces the whole edit. The file must be unchanged since the preview or the apply is rejected."
                },
                "tier": {
                    "type": "string",
                    "enum": ["l1", "l2"],
                    "description": "l1 (default): one-line apply result only. l2: also echoes the surrounding region of the primary file.",
                    "default": "l1"
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
        let dry_run = extract_bool(&args, "dry_run", false);

        if dry_run {
            // Delegate to preview, but wrap the preview payload in an explicit
            // dry-run envelope (see `dry_run_envelope`).
            let preview = EditPreviewHandler.execute(registry, args).await?;
            return Ok(dry_run_envelope(preview));
        }

        // validate:"check" refuses here, before anything is written. This is
        // the ONE integration point for lane F's check_scoped.
        let validate_request = edit_validate::resolve_validate_arg(&args)?;
        let had_validate_arg = args.get("validate").is_some();

        let request = apply_request_args(&args)?;
        let project_path_arg = args.get("project_path").and_then(Value::as_str);
        let handle = registry.get_or_create(project_path_arg).await?;

        // 0. Best-effort PDG load: applying a plain text edit must never be
        // blocked by a degraded/unavailable index.
        let pdg_loaded = load_pdg_best_effort(&handle).await;
        let pdg_snapshot = {
            let guard = handle.read().await;
            guard.pdg().cloned()
        };

        let (project_root, storage_path) = {
            let guard = handle.read().await;
            (
                guard.project_path().to_path_buf(),
                guard.storage_path().to_path_buf(),
            )
        };

        // 1. Acquire the edit: cached preview (token) or fresh text.
        enum Acquired {
            /// Single file: (canonical, original, modified, changes, token)
            Single(
                std::path::PathBuf,
                String,
                String,
                Vec<crate::edit::EditChange>,
                Option<String>,
            ),
            /// Multi-file batch plan + token.
            Batch(edit_batch::BatchEditPlan, Option<String>),
        }

        let acquired = match request {
            ApplyRequest::Token { token, file_path } => {
                // Token-only is the advertised path; with file_path also given,
                // resolve via the file (legacy clients) and verify the token.
                let entry = if let Some(path) = file_path.as_ref() {
                    let canonical = validate_file_within_project(path, &project_root)?;
                    let cached = GLOBAL_EDIT_CACHE.get(&storage_path, &canonical).await;
                    let entry = cached.ok_or_else(|| {
                        JsonRpcError::invalid_params(
                            "No cached preview found for this file — request a new \
                             LeIndex [Edit Preview] and apply with its token",
                        )
                    })?;
                    if entry.preview_token != token {
                        return Err(JsonRpcError::invalid_params(
                            "preview token mismatch — the token does not match the cached \
                             preview for this file. Request a new LeIndex [Edit Preview] \
                             and apply with its token",
                        ));
                    }
                    EditCacheEntry {
                        file_path: canonical,
                        ..entry
                    }
                } else {
                    GLOBAL_EDIT_CACHE
                        .get_by_token(&storage_path, &token)
                        .await
                        .ok_or_else(|| {
                            JsonRpcError::invalid_params(
                                "Unknown preview_token — request a new LeIndex [Edit Preview] \
                                 and apply with its token (preview tokens are single-use and \
                                 expire with the cache)",
                            )
                        })?
                };
                // §3.4: re-validate the file is unchanged since preview.
                if !entry.files_unchanged_since_preview() {
                    GLOBAL_EDIT_CACHE
                        .clear_with_token_alias(&storage_path, &entry.file_path, &token)
                        .await;
                    return Err(JsonRpcError::invalid_params(
                        "Edit rejected: file content changed since this preview was generated \
                         (hash mismatch). Request a new LeIndex [Edit Preview]; the stale \
                         token was discarded.",
                    ));
                }
                let multi_file = entry.changes.iter().any(|c| {
                    matches!(
                        c,
                        crate::edit::EditChange::ReplaceText {
                            file_path: Some(_),
                            ..
                        }
                    )
                });
                if multi_file {
                    let plan =
                        edit_batch::resolve_batch_from_changes(&entry.changes, &project_root)
                            .await?;
                    Acquired::Batch(plan, Some(token))
                } else {
                    let canonical = validate_file_within_project(
                        // Token-alias entries carry the synthetic `__token__:`
                        // cache key in `file_path`; the real path is in the
                        // preview's original-hashes map.
                        &entry.real_file_path().to_string_lossy(),
                        &project_root,
                    )?;
                    Acquired::Single(
                        canonical,
                        entry.original_text,
                        entry.modified_text,
                        entry.changes,
                        Some(token),
                    )
                }
            }
            ApplyRequest::Fresh { file_path, .. } => {
                let changes_val = fresh_changes_val(&args)?;
                let batch_mode = edit_batch::is_batch(&changes_val);
                if batch_mode {
                    let plan = edit_batch::resolve_batch(
                        &changes_val,
                        file_path.as_deref(),
                        &project_root,
                    )
                    .await?;
                    Acquired::Batch(plan, None)
                } else {
                    let Some(path) = file_path.as_deref() else {
                        return Err(JsonRpcError::invalid_params(
                            "Missing required argument: file_path",
                        ));
                    };
                    let canonical = validate_file_within_project(path, &project_root)?;
                    let original = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
                        JsonRpcError::invalid_params(format!("Cannot read file '{path}': {e}"))
                    })?;
                    let changes = parse_edit_changes(&changes_val, Some(&original))?;
                    let modified = apply_changes_in_memory(&original, &changes)?;
                    Acquired::Single(canonical, original, modified, changes, None)
                }
            }
        };

        // 2. Write. Batches validate every file before the first write and
        // roll back on failure; singles keep the expected-content guard.
        // `single_validation` carries the single-file validator report plus
        // its canonical path for the response.
        let (plan, _consumed_token, changes_len, single_validation): (
            edit_batch::BatchEditPlan,
            Option<String>,
            usize,
            Option<(Option<Value>, std::path::PathBuf)>,
        ) = match acquired {
            Acquired::Single(canonical, original, modified, changes, token) => {
                if modified == original {
                    if let Some(token) = token.as_ref() {
                        GLOBAL_EDIT_CACHE
                            .clear_with_token_alias(&storage_path, &canonical, token)
                            .await;
                    } else {
                        GLOBAL_EDIT_CACHE.clear(&storage_path, &canonical).await;
                    }
                    let response = serde_json::json!({
                        "success": true,
                        "changes_applied": 0,
                        "file_path": canonical.to_string_lossy(),
                        "message": "No changes to apply (content identical)"
                    });
                    return Ok(wrap_with_current_meta(&handle, response).await);
                }
                let validation_json = {
                    let guard = handle.read().await;
                    validate_edit(guard.create_validator(), &canonical, &original, &modified)
                };
                let success = apply_atomic(&canonical, &original, &modified).await?;
                ensure_write_succeeded(success, &storage_path, &canonical).await?;
                let plan = edit_batch::BatchEditPlan {
                    files: vec![edit_batch::BatchFileEdit {
                        canonical_path: canonical.clone(),
                        display_path: canonical.to_string_lossy().to_string(),
                        original,
                        modified,
                        changes: changes.clone(),
                    }],
                };
                if let Some(token) = token.as_ref() {
                    GLOBAL_EDIT_CACHE
                        .clear_with_token_alias(&storage_path, &canonical, token)
                        .await;
                } else {
                    GLOBAL_EDIT_CACHE.clear(&storage_path, &canonical).await;
                }
                (
                    plan,
                    token,
                    changes.len(),
                    Some((validation_json, canonical)),
                )
            }
            Acquired::Batch(plan, token) => {
                let changes_len = plan.change_count();
                edit_batch::apply_batch_atomic(&plan).await?;
                if let Some(token) = token.as_ref() {
                    let primary = plan
                        .files
                        .first()
                        .map(|f| f.canonical_path.clone())
                        .unwrap_or_else(|| std::path::PathBuf::from(""));
                    GLOBAL_EDIT_CACHE
                        .clear_with_token_alias(&storage_path, &primary, token)
                        .await;
                }
                (plan, token, changes_len, None)
            }
        };

        // 3. Invalidate the registry's staleness cache so the next
        // read tool re-runs `is_stale_fast` instead of reusing a
        // pre-write `false` cached result.
        registry.invalidate_stale_cache(&project_root).await;

        // 4. Build the response NOW — the edit is durable (see the
        // latency note in refresh_index_after_edit below).
        let (validation_json, _validation_path) = match &single_validation {
            Some((validation, path)) => (validation.clone(), Some(path.clone())),
            None => (None, None),
        };
        let impact = {
            let guard = handle.read().await;
            edit_impact(
                guard.pdg(),
                &plan
                    .files
                    .iter()
                    .flat_map(|f| f.changes.clone())
                    .collect::<Vec<_>>(),
                plan.files
                    .first()
                    .map(|f| f.canonical_path.as_path())
                    .unwrap_or_else(|| std::path::Path::new("")),
            )
        };

        let primary_path = plan
            .files
            .first()
            .map(|f| f.canonical_path.clone())
            .unwrap_or_else(|| std::path::PathBuf::from(""));
        let mut response = serde_json::json!({
            "success": true,
            "changes_applied": changes_len,
            "file_path": primary_path.to_string_lossy(),
            "applied_lines": applied_lines(&plan.files, pdg_snapshot.as_ref()),
            "files": plan.files.iter().map(file_summary).collect::<Vec<_>>(),
            "multi_file": plan.files.len() > 1,
        });
        // §4.3: the surrounding-region echo is OFF by default — the caller
        // just wrote that text. It comes back only on `tier=l2`.
        if args
            .get("tier")
            .and_then(Value::as_str)
            .is_some_and(|tier| tier.eq_ignore_ascii_case("l2"))
        {
            response["edit_region"] = serde_json::json!(
                plan.files
                    .first()
                    .map(|f| edit_region(&f.original, &f.modified))
                    .unwrap_or_default()
            );
        }
        if let (Some(validation), Some(obj)) = (validation_json, response.as_object_mut()) {
            obj.insert("validation".to_string(), validation);
        }
        let (nodes, files, breaking) = impact;
        let mut affected_files: Vec<_> = files.into_iter().collect();
        affected_files.sort();
        response["affected_symbols"] = serde_json::json!(nodes);
        response["affected_files"] = serde_json::json!(affected_files);
        response["breaking_changes"] = serde_json::json!(breaking);

        // validate:"syntax" — tree-sitter parse of every edited file, after
        // the writes are durable.
        if had_validate_arg && matches!(validate_request, ValidateRequest::Syntax) {
            let files_to_check: Vec<(String, std::path::PathBuf, String)> = plan
                .files
                .iter()
                .map(|f| {
                    (
                        f.display_path.clone(),
                        f.canonical_path.clone(),
                        f.modified.clone(),
                    )
                })
                .collect();
            let outcomes = edit_validate::validate_syntax(&files_to_check, 5);
            response["syntax"] = serde_json::json!(
                outcomes
                    .iter()
                    .map(|o| serde_json::json!({
                        "path": o.display_path,
                        "summary": o.summary,
                        "errors": o.errors,
                    }))
                    .collect::<Vec<_>>()
            );
        }

        if !pdg_loaded {
            annotate_degraded_pdg(&mut response);
        }

        let response = wrap_with_current_meta(&handle, response).await;

        // 5. Index maintenance AFTER the response value exists: the caller
        // receives the response now, and the refresh never rides on its
        // latency (see `refresh_index_after_edit`).
        refresh_index_after_edit(registry, &handle).await;

        Ok(response)
    }
}

/// Extract the FRESH-mode changes value (changes[] or old/new aliases).
fn fresh_changes_val(args: &Value) -> Result<Value, JsonRpcError> {
    if let Some(changes) = args.get("changes").cloned() {
        return Ok(changes);
    }
    let old_text = args
        .get("old_text")
        .or_else(|| args.get("old_str"))
        .and_then(Value::as_str);
    let new_text = args
        .get("new_text")
        .or_else(|| args.get("new_str"))
        .and_then(Value::as_str);
    match (old_text, new_text) {
        (Some(old), Some(new)) => Ok(serde_json::json!([{
            "type": "replace_text",
            "old_text": old,
            "new_text": new
        }])),
        _ => Err(JsonRpcError::invalid_params(
            "Provide either 'changes' array or 'old_text'+'new_text' for simple replacement",
        )),
    }
}
