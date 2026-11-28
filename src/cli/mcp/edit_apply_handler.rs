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

/// Applied-line echo math (§4.3): original→final offset translation so the
/// `Applied: path:Lx-Ly` echo never slices `modified` text at stale original
/// byte offsets (multi-byte UTF-8 panic, wrong ranges).
#[path = "edit_apply_applied.rs"]
mod edit_apply_applied;
use edit_apply_applied::applied_lines;

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

/// The session key the read ledger records edits under (§4.1): the transport
/// injects `_session` (the MCP session id); direct CLI calls land under the
/// process-scoped key, matching how reads record.
fn session_id_for_ledger(args: &Value) -> &str {
    args.get("_session")
        .and_then(Value::as_str)
        .unwrap_or("__process__")
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
    files: std::collections::HashSet<String>,
) -> EditImpact {
    let mut nodes = Vec::new();
    let mut files = files;
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
            .or_else(|| pdg.find_by_name_in_file(old_name, None));
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

/// Handler for LeIndex [edit_apply — atomic code modifications.
#[derive(Clone)]
pub struct EditApplyHandler;

/// Drop a consumed preview from the cache.
///
/// A token-backed preview is stored under BOTH the path key and a token
/// alias, so both must go: clearing only the path key would leave the alias
/// replayable. Without a token there is only the path key.
async fn clear_consumed_preview(
    storage_path: &std::path::Path,
    canonical: &std::path::Path,
    token: Option<&String>,
) {
    match token {
        Some(token) => {
            GLOBAL_EDIT_CACHE
                .clear_with_token_alias(storage_path, canonical, token)
                .await
        }
        None => GLOBAL_EDIT_CACHE.clear(storage_path, canonical).await,
    }
}

/// The canonical path of a batch plan's primary file (empty for a plan with no
/// files, which no valid plan produces).
fn primary_canonical(plan: &edit_batch::BatchEditPlan) -> std::path::PathBuf {
    plan.files
        .first()
        .map(|f| f.canonical_path.clone())
        .unwrap_or_else(|| std::path::PathBuf::from(""))
}

/// What a written edit produced, for the response stage.
struct Applied {
    plan: edit_batch::BatchEditPlan,
    consumed_token: Option<String>,
    changes_len: usize,
    /// Single-file validator report plus its canonical path.
    single_validation: Option<(Option<Value>, std::path::PathBuf)>,
}

/// The `changes_applied: 0` answer for an edit whose content is identical to
/// what is on disk. The response is built here so both arms agree on it.
fn noop_response(canonical: &std::path::Path) -> Value {
    serde_json::json!({
        "success": true,
        "changes_applied": 0,
        "file_path": canonical.to_string_lossy(),
        "message": "No changes to apply (content identical)"
    })
}

/// Write a single-file edit: validate, write atomically with the
/// expected-content guard, drop the consumed preview, and describe it as a
/// one-file plan. `Ok(None)` means the content was already identical.
async fn write_single(
    handle: &ProjectHandle,
    storage_path: &std::path::Path,
    canonical: std::path::PathBuf,
    original: String,
    modified: String,
    changes: Vec<crate::edit::EditChange>,
    token: Option<String>,
) -> Result<Option<Applied>, JsonRpcError> {
    if modified == original {
        clear_consumed_preview(storage_path, &canonical, token.as_ref()).await;
        return Ok(None);
    }
    let validation_json = {
        let guard = handle.read().await;
        validate_edit(guard.create_validator(), &canonical, &original, &modified)
    };
    let success = apply_atomic(&canonical, &original, &modified).await?;
    ensure_write_succeeded(success, storage_path, &canonical).await?;
    let plan = edit_batch::BatchEditPlan {
        files: vec![edit_batch::BatchFileEdit {
            canonical_path: canonical.clone(),
            display_path: canonical.to_string_lossy().to_string(),
            original,
            modified,
            changes: changes.clone(),
        }],
    };
    clear_consumed_preview(storage_path, &canonical, token.as_ref()).await;
    Ok(Some(Applied {
        plan,
        consumed_token: token,
        changes_len: changes.len(),
        single_validation: Some((validation_json, canonical)),
    }))
}

/// Write a multi-file batch atomically, or report that nothing needed writing.
/// `Ok(None)` means every file was already identical, so the parsed change
/// count must not be reported as applied.
async fn write_batch(
    storage_path: &std::path::Path,
    plan: edit_batch::BatchEditPlan,
    token: Option<String>,
) -> Result<Option<Applied>, JsonRpcError> {
    if plan.files.iter().all(|f| f.modified == f.original) {
        if let Some(token) = token.as_ref() {
            let primary = primary_canonical(&plan);
            GLOBAL_EDIT_CACHE
                .clear_with_token_alias(storage_path, &primary, token)
                .await;
        }
        return Ok(None);
    }
    let changes_len = plan.change_count();
    edit_batch::apply_batch_atomic(&plan).await?;
    if let Some(token) = token.as_ref() {
        let primary = primary_canonical(&plan);
        GLOBAL_EDIT_CACHE
            .clear_with_token_alias(storage_path, &primary, token)
            .await;
    }
    Ok(Some(Applied {
        plan,
        consumed_token: token,
        changes_len,
        single_validation: None,
    }))
}

/// An edit resolved to text: either one file or a multi-file batch.
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

/// Resolve the caller's request into edit text: a cached preview (by token)
/// or the fresh text it resent.
///
/// A token-backed preview must still be UNCHANGED on disk since it was
/// generated (§3.4) — the hash check reads and hashes every touched file, so it
/// runs on the blocking pool rather than parking the async executor behind disk
/// I/O.
async fn acquire_edit(
    request: ApplyRequest,
    args: &Value,
    project_root: &std::path::Path,
    storage_path: &std::path::Path,
) -> Result<Acquired, JsonRpcError> {
    match request {
        ApplyRequest::Token { token, file_path } => {
            acquire_from_token(token, file_path, project_root, storage_path).await
        }
        ApplyRequest::Fresh { file_path } => acquire_fresh(args, file_path, project_root).await,
    }
}

/// Resolve a token to edit text.
///
/// Token-only is the advertised path; with `file_path` also given, resolve via
/// the file (legacy clients) and verify the token.
async fn acquire_from_token(
    token: String,
    file_path: Option<String>,
    project_root: &std::path::Path,
    storage_path: &std::path::Path,
) -> Result<Acquired, JsonRpcError> {
    let entry = match file_path.as_ref() {
        Some(path) => {
            let canonical = validate_file_within_project(path, project_root)?;
            let entry = GLOBAL_EDIT_CACHE
                .get(storage_path, &canonical)
                .await
                .ok_or_else(|| {
                    JsonRpcError::invalid_params(
                        "No cached preview found for this file — request a new \
                         LeIndex [Edit Preview] and apply with its token",
                    )
                })?;
            if entry.preview_token != token {
                // The cached preview under this path belongs to a different
                // token — discard BOTH lookup entries so the stale primary can
                // never be replayed through a mismatched token.
                GLOBAL_EDIT_CACHE
                    .clear_with_token_alias(storage_path, &canonical, &token)
                    .await;
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
        }
        None => GLOBAL_EDIT_CACHE
            .get_by_token(storage_path, &token)
            .await
            .ok_or_else(|| {
                JsonRpcError::invalid_params(
                    "Unknown preview_token — request a new LeIndex [Edit Preview] \
                     and apply with its token (preview tokens are single-use and \
                     expire with the cache)",
                )
            })?,
    };
    let entry_for_check = entry.clone();
    let unchanged =
        tokio::task::spawn_blocking(move || entry_for_check.files_unchanged_since_preview())
            .await
            .map_err(|e| {
                JsonRpcError::internal_error(format!("Staleness check task failed: {e}"))
            })?;
    if !unchanged {
        GLOBAL_EDIT_CACHE
            .clear_with_token_alias(storage_path, &entry.file_path, &token)
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
        let plan = edit_batch::resolve_batch_from_changes(
            &entry.changes,
            &entry.original_hashes,
            project_root,
        )
        .await?;
        return Ok(Acquired::Batch(plan, Some(token)));
    }
    let canonical = validate_file_within_project(
        // Token-alias entries carry the synthetic `__token__:` cache key in
        // `file_path`; the real path is in the preview's original-hashes map.
        &entry.real_file_path().to_string_lossy(),
        project_root,
    )?;
    Ok(Acquired::Single(
        canonical,
        entry.original_text,
        entry.modified_text,
        entry.changes,
        Some(token),
    ))
}

/// Resolve resent edit text (no token): a batch when the changes[] entries
/// span files, otherwise a single file read from disk.
async fn acquire_fresh(
    args: &Value,
    file_path: Option<String>,
    project_root: &std::path::Path,
) -> Result<Acquired, JsonRpcError> {
    let changes_val = fresh_changes_val(args)?;
    if edit_batch::is_batch(&changes_val) {
        let plan =
            edit_batch::resolve_batch(&changes_val, file_path.as_deref(), project_root).await?;
        return Ok(Acquired::Batch(plan, None));
    }
    let Some(path) = file_path.as_deref() else {
        return Err(JsonRpcError::invalid_params(
            "Missing required argument: file_path",
        ));
    };
    let canonical = validate_file_within_project(path, project_root)?;
    let original = tokio::fs::read_to_string(&canonical)
        .await
        .map_err(|e| JsonRpcError::invalid_params(format!("Cannot read file '{path}': {e}")))?;
    let changes = parse_edit_changes(&changes_val, Some(&original))?;
    let modified = apply_changes_in_memory(&original, &changes)?;
    Ok(Acquired::Single(
        canonical, original, modified, changes, None,
    ))
}

/// `validate:"syntax"` echo: tree-sitter parse of every edited file, run AFTER
/// the writes are durable.
fn attach_syntax_validation(response: &mut Value, plan: &edit_batch::BatchEditPlan) {
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

/// `validate:"check"` echo: the scoped check (cargo/tsc) on the owning package
/// of each edited file (§4.3, cross-lane contract).
///
/// Failures to RUN a check never fail an applied edit; they come back as
/// per-file `error` entries. `check_scoped` is BLOCKING (it spawns its own
/// runtime and waits on a child process), so it runs on the blocking pool,
/// exactly like the sibling call site in `run::actions`.
async fn attach_scoped_checks(
    response: &mut Value,
    plan: &edit_batch::BatchEditPlan,
    project_root: &std::path::Path,
) {
    let mut checks = Vec::new();
    for file in plan.files.iter().map(|f| f.canonical_path.clone()) {
        let (root, target) = (project_root.to_path_buf(), file.clone());
        let outcome = tokio::task::spawn_blocking(move || {
            crate::cli::run::diagnostics::check_scoped(&root, &target, 10)
        })
        .await;
        match outcome {
            Ok(Ok(summary)) => checks.push(serde_json::json!({
                "file": file.to_string_lossy(),
                "exit_code": summary.exit_code,
                "headline": summary.headline,
                "failures": summary.failures.iter().map(|failure| serde_json::json!({
                    "name": failure.name,
                    "detail": failure.detail,
                })).collect::<Vec<_>>(),
                "error_count": summary.error_count,
                "warning_counts": summary.warning_counts,
            })),
            Ok(Err(error)) => checks.push(serde_json::json!({
                "file": file.to_string_lossy(),
                "error": error.to_string(),
            })),
            Err(error) => checks.push(serde_json::json!({
                "file": file.to_string_lossy(),
                "error": format!("check task failed: {error}"),
            })),
        }
    }
    response["check"] = serde_json::json!({ "files": checks });
}

/// Everything after the write: staleness invalidation, the response payload,
/// the optional validation echoes, the session ledger, and index maintenance.
///
/// The response is built and returned BEFORE the index refresh: the edit is
/// already durable, so the caller must not wait on a reindex (see
/// [`refresh_index_after_edit`]).
#[allow(clippy::too_many_arguments)]
async fn finish_apply(
    registry: &Arc<ProjectRegistry>,
    handle: &ProjectHandle,
    project_root: &std::path::Path,
    plan: edit_batch::BatchEditPlan,
    changes_len: usize,
    single_validation: Option<(Option<Value>, std::path::PathBuf)>,
    pdg_snapshot: Option<crate::graph::pdg::ProgramDependenceGraph>,
    pdg_loaded: bool,
    had_validate_arg: bool,
    validate_request: ValidateRequest,
    args: &Value,
) -> Result<Value, JsonRpcError> {
    // Invalidate the registry's staleness cache so the next read tool re-runs
    // `is_stale_fast` instead of reusing a pre-write `false` cached result.
    registry.invalidate_stale_cache(project_root).await;

    let validation_json = single_validation.and_then(|(validation, _path)| validation);
    let impact = {
        let guard = handle.read().await;
        let files = plan
            .files
            .iter()
            .map(|file| file.canonical_path.to_string_lossy().to_string())
            .collect::<std::collections::HashSet<_>>();
        edit_impact(
            guard.pdg(),
            &plan
                .files
                .iter()
                .flat_map(|f| f.changes.clone())
                .collect::<Vec<_>>(),
            files,
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
    // §4.3: the surrounding-region echo is OFF by default — the caller just
    // wrote that text. It comes back only on `tier=l2`.
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

    // validate:"syntax" / "check" echoes, after the writes are durable.
    if had_validate_arg {
        match validate_request {
            ValidateRequest::Syntax => attach_syntax_validation(&mut response, &plan),
            ValidateRequest::Check => {
                attach_scoped_checks(&mut response, &plan, project_root).await
            }
            ValidateRequest::None => {}
        }
    }

    // §4.1/§6.9 (lane E): a durable edit invalidates this session's cached
    // reads for every touched file. Fire-and-forget: ledger failures can never
    // fail an edit.
    let session = session_id_for_ledger(args);
    for file in plan.files.iter().filter(|f| f.modified != f.original) {
        crate::cli::mcp::read_paths::session_ledger().record_edit(session, &file.canonical_path);
    }

    if !pdg_loaded {
        annotate_degraded_pdg(&mut response);
    }
    let response = wrap_with_current_meta(handle, response).await;

    // Index maintenance AFTER the response value exists: the caller receives
    // the response now, and the refresh never rides on its latency.
    refresh_index_after_edit(registry, handle).await;
    Ok(response)
}

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
                    "enum": ["syntax", "check"],
                    "description": "\"syntax\": tree-sitter-parse every edited file after the write and report errors (line:col). \"check\": run the package-scoped cargo/tsc check (lane F) and surface its diagnostics in the apply echo."
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

        // The `tier` argument is declared as an enum in the schema; validate
        // it explicitly so an unknown value errors instead of silently
        // downgrading to l1 (region echo skipped) with no signal to the caller.
        if let Some(tier) = args.get("tier").and_then(Value::as_str)
            && !tier.eq_ignore_ascii_case("l1")
            && !tier.eq_ignore_ascii_case("l2")
        {
            return Err(JsonRpcError::invalid_params(format!(
                "tier must be \"l1\" or \"l2\", got: {tier:?}"
            )));
        }

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
        let acquired = acquire_edit(request, &args, &project_root, &storage_path).await?;

        // 2. Write. Batches validate every file before the first write and
        // roll back on failure; singles keep the expected-content guard.
        // `single_validation` carries the single-file validator report plus
        // its canonical path for the response.
        let applied = match acquired {
            Acquired::Single(canonical, original, modified, changes, token) => {
                let noop_path = canonical.clone();
                match write_single(
                    &handle,
                    &storage_path,
                    canonical,
                    original,
                    modified,
                    changes,
                    token,
                )
                .await?
                {
                    Some(applied) => applied,
                    None => {
                        let response = noop_response(&noop_path);
                        return Ok(wrap_with_current_meta(&handle, response).await);
                    }
                }
            }
            Acquired::Batch(plan, token) => {
                let noop_path = primary_canonical(&plan);
                match write_batch(&storage_path, plan, token).await? {
                    Some(applied) => applied,
                    None => {
                        let response = noop_response(&noop_path);
                        return Ok(wrap_with_current_meta(&handle, response).await);
                    }
                }
            }
        };
        let Applied {
            plan,
            consumed_token: _consumed_token,
            changes_len,
            single_validation,
        } = applied;

        finish_apply(
            registry,
            &handle,
            &project_root,
            plan,
            changes_len,
            single_validation,
            pdg_snapshot,
            pdg_loaded,
            had_validate_arg,
            validate_request,
            &args,
        )
        .await
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

#[cfg(test)]
#[path = "edit_apply_handler_test.rs"]
mod edit_apply_tests;
