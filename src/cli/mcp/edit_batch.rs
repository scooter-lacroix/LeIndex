//! Multi-file atomic batch edits (§4.3).
//!
//! A batch is a `changes[]` whose entries carry their own `file_path`.
//! Every file is resolved and edited in memory FIRST; only when every
//! change of every file validated does anything touch the disk. Writes
//! are atomic (temp file + persist) and, if a write fails after another
//! file was already written, the already-written files are restored —
//! all-or-nothing.

use super::helpers::{apply_changes_in_memory, parse_edit_changes, validate_file_within_project};
use super::protocol::JsonRpcError;
use crate::edit::EditChange;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One file of a validated batch: pre-edit content, post-edit content.
pub(crate) struct BatchFileEdit {
    /// Canonical path (resolved against the project root).
    pub canonical_path: PathBuf,
    /// The path as the caller named it (used for display).
    pub display_path: String,
    /// File content the changes were computed against.
    pub original: String,
    /// File content after applying the changes in memory.
    pub modified: String,
    /// The parsed changes belonging to this file.
    pub changes: Vec<EditChange>,
}

/// A fully validated multi-file edit plan. Nothing has been written yet.
pub(crate) struct BatchEditPlan {
    /// One entry per touched file, in first-mention order.
    pub files: Vec<BatchFileEdit>,
}

impl BatchEditPlan {
    /// Total parsed changes across all files.
    pub fn change_count(&self) -> usize {
        self.files.iter().map(|f| f.changes.len()).sum()
    }
}

/// True when the parsed `changes[]` is a multi-file batch: at least one
/// entry names its own `file_path`.
pub(crate) fn is_batch(changes_val: &Value) -> bool {
    changes_val
        .as_array()
        .is_some_and(|arr| {
            arr.iter()
                .any(|item| item.get("file_path").and_then(Value::as_str).is_some())
        })
}

/// Resolve a batch `changes[]` into per-file plans. Entries without their
/// own `file_path` belong to `default_file` (the request's top-level path).
/// Every change is parsed against its own file's on-disk content, so a
/// missing `old_text` in file 2 fails the whole batch before file 1 is
/// ever written.
pub(crate) async fn resolve_batch(
    changes_val: &Value,
    default_file: Option<&str>,
    project_root: &std::path::Path,
) -> Result<BatchEditPlan, JsonRpcError> {
    let arr = changes_val
        .as_array()
        .ok_or_else(|| JsonRpcError::invalid_params("'changes' must be an array"))?;

    // Group change indices by owning file, preserving request order.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, item) in arr.iter().enumerate() {
        let owner = item
            .get("file_path")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| default_file.map(str::to_owned))
            .ok_or_else(|| {
                JsonRpcError::invalid_params(format!(
                    "changes[{index}]: missing 'file_path' — in a batch every entry must \
                     carry 'file_path', or the request must pass a top-level 'file_path'"
                ))
            })?;
        groups.entry(owner).or_default().push(index);
    }

    let mut files = Vec::with_capacity(groups.len());
    for (owner, indices) in groups {
        let canonical = validate_file_within_project(&owner, project_root)?;
        let original = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{owner}': {e}"))
        })?;
        let file_items: Vec<Value> = indices.iter().map(|&i| arr[i].clone()).collect();
        if file_items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some("rename_symbol"))
        {
            return Err(JsonRpcError::invalid_params(
                "rename_symbol is not supported inside a multi-file batch — use the rename action",
            ));
        }
        let changes = parse_edit_changes(&Value::Array(file_items), Some(&original))?;
        let modified = apply_changes_in_memory(&original, &changes)?;
        files.push(BatchFileEdit {
            canonical_path: canonical,
            display_path: owner,
            original,
            modified,
            changes,
        });
    }
    Ok(BatchEditPlan { files })
}

/// Rebuild a batch plan from a cached preview's changes. The file content is
/// re-read (staleness is re-validated by the caller via the cached hashes)
/// and the changes are re-applied in memory.
pub(crate) async fn resolve_batch_from_changes(
    changes: &[EditChange],
    project_root: &std::path::Path,
) -> Result<BatchEditPlan, JsonRpcError> {
    // Re-group the cached changes by their owning file.
    let mut groups: BTreeMap<String, Vec<&EditChange>> = BTreeMap::new();
    for change in changes {
        let crate::edit::EditChange::ReplaceText {
            file_path: Some(owner),
            ..
        } = change
        else {
            return Err(JsonRpcError::invalid_params(
                "Cached batch preview contains a change without a file_path",
            ));
        };
        groups.entry(owner.clone()).or_default().push(change);
    }

    let mut files = Vec::with_capacity(groups.len());
    for (owner, owned) in groups {
        let canonical = validate_file_within_project(&owner, project_root)?;
        let original = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{owner}': {e}"))
        })?;
        let changes: Vec<EditChange> = owned.into_iter().cloned().collect();
        let modified = apply_changes_in_memory(&original, &changes)?;
        files.push(BatchFileEdit {
            canonical_path: canonical,
            display_path: owner,
            original,
            modified,
            changes,
        });
    }
    Ok(BatchEditPlan { files })
}

/// Write every file of a validated plan, all-or-nothing.
///
/// 1. Every file is re-read and compared against its previewed `original`;
///    any drift rejects the whole batch BEFORE the first write.
/// 2. Files are written through the atomic (temp + persist) writer in plan
///    order. A write failure restores the already-written files to their
///    original bytes before erroring.
pub(crate) async fn apply_batch_atomic(plan: &BatchEditPlan) -> Result<(), JsonRpcError> {
    for file in &plan.files {
        let current = tokio::fs::read(&file.canonical_path).await.map_err(|e| {
            JsonRpcError::internal_error(format!(
                "Failed to read '{}' before batch write: {e}",
                file.canonical_path.display()
            ))
        })?;
        if current != file.original.as_bytes() {
            return Err(JsonRpcError::invalid_params(format!(
                "Edit rejected: '{}' changed on disk since the preview was generated. \
                 Request a new LeIndex [Edit Preview].",
                file.canonical_path.display()
            )));
        }
    }

    let mut written: Vec<&BatchFileEdit> = Vec::new();
    for file in &plan.files {
        if file.modified == file.original {
            continue;
        }
        let result = crate::edit::atomic_write_async(
            file.canonical_path.clone(),
            file.modified.as_bytes().to_vec(),
        )
        .await;
        if let Err(error) = result {
            for done in written.iter().rev() {
                if let Err(restore_error) = crate::edit::atomic_write_async(
                    done.canonical_path.clone(),
                    done.original.as_bytes().to_vec(),
                )
                .await
                {
                    tracing::error!(
                        file = %done.canonical_path.display(),
                        "Batch rollback failed: {restore_error} — file left in edited state"
                    );
                }
            }
            return Err(JsonRpcError::internal_error(format!(
                "Batch write failed at '{}' (earlier files rolled back): {error}",
                file.canonical_path.display()
            )));
        }
        written.push(file);
    }
    Ok(())
}
