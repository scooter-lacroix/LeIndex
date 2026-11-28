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
    changes_val.as_array().is_some_and(|arr| {
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

    // Group change indices by CANONICAL file identity, preserving first-mention
    // order. Raw spellings ("a.rs" vs "./a.rs" vs an absolute path) must merge
    // into ONE plan: two plans for the same canonical file would each pass the
    // pre-write checks against the original content, and the second write
    // would silently overwrite the first edit while both are reported as
    // applied.
    let mut groups: Vec<(std::path::PathBuf, String, Vec<usize>)> = Vec::new();
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
        let canonical = validate_file_within_project(&owner, project_root)?;
        if let Some((_, _, indices)) = groups.iter_mut().find(|(c, _, _)| c == &canonical) {
            indices.push(index);
        } else {
            groups.push((canonical, owner, vec![index]));
        }
    }

    let mut files = Vec::with_capacity(groups.len());
    for (canonical, owner, indices) in groups {
        let original = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{owner}': {e}"))
        })?;
        // Write the resolved owner into every item before parsing. An entry
        // without its own `file_path` inherits the top-level one only for
        // GROUPING above; the parsed change would otherwise carry
        // `file_path: None`, the preview caches that, and a token-only apply
        // then rejects the batch as "multi-file with a change without a
        // file_path" and writes nothing.
        let file_items: Vec<Value> = indices
            .iter()
            .map(|&i| {
                let mut item = arr[i].clone();
                if item.get("file_path").and_then(Value::as_str).is_none()
                    && let Some(obj) = item.as_object_mut()
                {
                    obj.insert("file_path".to_string(), Value::String(owner.clone()));
                }
                item
            })
            .collect();
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
    integrity_hashes: &std::collections::HashMap<String, String>,
    project_root: &std::path::Path,
) -> Result<BatchEditPlan, JsonRpcError> {
    // Re-group the cached changes by CANONICAL file identity, preserving
    // first-mention order (same merge rationale as resolve_batch).
    let mut groups: Vec<(std::path::PathBuf, String, Vec<&EditChange>)> = Vec::new();
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
        let canonical = validate_file_within_project(owner, project_root)?;
        if let Some((_, _, owned)) = groups.iter_mut().find(|(c, _, _)| c == &canonical) {
            owned.push(change);
        } else {
            groups.push((canonical, owner.clone(), vec![change]));
        }
    }

    let mut files = Vec::with_capacity(groups.len());
    for (canonical, owner, owned) in groups {
        let original = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            JsonRpcError::invalid_params(format!("Cannot read file '{owner}': {e}"))
        })?;
        // Replay ONLY against the bytes bound to the preview: without a
        // matching integrity hash the cached offsets are untrustworthy
        // (legacy entries written before hash tracking, or disk drift).
        let key = canonical.to_string_lossy().to_string();
        let Some(expected) = integrity_hashes.get(&key) else {
            return Err(JsonRpcError::invalid_params(format!(
                "Cached batch preview for '{owner}' lacks integrity hashes — \
                 request a new LeIndex [Edit Preview]."
            )));
        };
        let actual = blake3::hash(original.as_bytes()).to_hex().to_string();
        if actual != *expected {
            return Err(JsonRpcError::invalid_params(format!(
                "Cached batch preview is stale for '{owner}' (content changed since \
                 preview) — request a new LeIndex [Edit Preview]."
            )));
        }
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
///    original bytes before erroring; restore failures are reported.
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

    // Writes + rollback run as ONE blocking unit: dropping the calling future
    // mid-sequence cannot strand a half-written batch (spawn_blocking tasks
    // run to completion even after their JoinHandle is dropped).
    let writes: Vec<(PathBuf, Vec<u8>, Vec<u8>)> = plan
        .files
        .iter()
        .filter(|f| f.modified != f.original)
        .map(|f| {
            (
                f.canonical_path.clone(),
                f.modified.as_bytes().to_vec(),
                f.original.as_bytes().to_vec(),
            )
        })
        .collect();

    let outcome = tokio::task::spawn_blocking(move || {
        let mut written: Vec<usize> = Vec::new();
        for (idx, (target, data, expected)) in writes.iter().enumerate() {
            let result = write_with_expected(target, data, expected);
            if let Err(error) = result {
                let mut failed_restores = Vec::new();
                for &done in written.iter().rev() {
                    let (prev_path, _, prev_expected) = &writes[done];
                    if let Err(restore_error) = crate::edit::atomic_write(prev_path, prev_expected)
                    {
                        failed_restores.push(format!("{} ({restore_error})", prev_path.display()));
                    }
                }
                return Err(BatchWriteFailure {
                    target: target.clone(),
                    error,
                    failed_restores,
                });
            }
            written.push(idx);
        }
        Ok(())
    })
    .await
    .map_err(|e| JsonRpcError::internal_error(format!("Batch write task failed: {e}")))?;

    match outcome {
        Ok(()) => Ok(()),
        Err(failure) => Err(failure.into_json_rpc_error()),
    }
}

/// Atomic write guarded by expected prior content. Returns `Err` when the
/// file drifted since the preview (the write is NOT attempted) or when the
/// write itself fails.
fn write_with_expected(target: &PathBuf, data: &[u8], expected: &[u8]) -> std::io::Result<()> {
    if target.exists() {
        let current = std::fs::read(target)?;
        if current != expected {
            return Err(std::io::Error::other(
                "file content changed since preview (drift detected at write time)",
            ));
        }
    } else if !expected.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file disappeared since preview",
        ));
    }
    crate::edit::atomic_write(target, data)
}

struct BatchWriteFailure {
    target: PathBuf,
    error: std::io::Error,
    failed_restores: Vec<String>,
}

impl BatchWriteFailure {
    fn into_json_rpc_error(self) -> JsonRpcError {
        let mut message = format!(
            "Batch write failed at '{}' (earlier files rolled back): {}",
            self.target.display(),
            self.error
        );
        if !self.failed_restores.is_empty() {
            message.push_str(&format!(
                " — WARNING: restoring earlier files failed for: {}",
                self.failed_restores.join(", ")
            ));
        }
        JsonRpcError::internal_error(message)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// §4.3: every change of every file is validated BEFORE the first
    /// write — a bad `old_text` in file 2 fails the batch and file 1 is
    /// never touched on disk.
    #[tokio::test]
    async fn test_batch_rejects_before_any_write_when_later_file_old_text_missing() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("one.rs");
        let f2 = dir.path().join("two.rs");
        std::fs::write(&f1, "fn one() {}\n").unwrap();
        std::fs::write(&f2, "fn two() {}\n").unwrap();
        let changes = serde_json::json!([
            {
                "file_path": f1.to_str().unwrap(),
                "type": "replace_text",
                "old_text": "fn one() {}",
                "new_text": "fn one_renamed() {}"
            },
            {
                "file_path": f2.to_str().unwrap(),
                "type": "replace_text",
                "old_text": "THIS TEXT IS NOT IN THE FILE",
                "new_text": "x"
            }
        ]);
        let result = resolve_batch(&changes, None, dir.path()).await;
        assert!(
            result.is_err(),
            "missing old_text in file 2 must fail the whole batch"
        );
        assert_eq!(
            std::fs::read_to_string(&f1).unwrap(),
            "fn one() {}\n",
            "all-or-nothing: file 1 must be untouched when file 2 fails validation"
        );
        assert_eq!(
            std::fs::read_to_string(&f2).unwrap(),
            "fn two() {}\n",
            "file 2 must be untouched"
        );
    }

    /// §4.3: a write failure mid-batch rolls back the already-written
    /// files, leaving every file at its pre-batch content.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_batch_write_failure_rolls_back_earlier_files() {
        use std::os::unix::fs::PermissionsExt;
        // The failure is injected by making file 2's DIRECTORY read-only so
        // its temp+rename write fails after file 1 was already written.
        // POSIX mode bits do not bind uid 0, so this injection is
        // impossible to synthesize deterministically as root — the test is
        // meaningful for every non-root developer/CI user.
        if running_as_root() {
            eprintln!("skipping: effective uid is root (mode bits cannot inject failure)");
            return;
        }
        let readonly_dir = tempfile::tempdir().unwrap();
        let writable_dir = tempfile::tempdir().unwrap();
        let f1 = writable_dir.path().join("one.rs");
        let f2 = readonly_dir.path().join("two.rs");
        std::fs::write(&f1, "fn one() {}\n").unwrap();
        std::fs::write(&f2, "fn two() {}\n").unwrap();

        // File 2's DIRECTORY goes read-only so its temp+rename write
        // fails after file 1 was already written.
        let mut perms = std::fs::metadata(readonly_dir.path())
            .unwrap()
            .permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(readonly_dir.path(), perms).unwrap();

        let plan = BatchEditPlan {
            files: vec![
                BatchFileEdit {
                    canonical_path: f1.clone(),
                    display_path: f1.to_string_lossy().to_string(),
                    original: "fn one() {}\n".to_string(),
                    modified: "fn one_edited() {}\n".to_string(),
                    changes: parse_edit_changes(
                        &serde_json::json!([
                            {"type": "replace_text", "old_text": "fn one() {}", "new_text": "fn one_edited() {}"}
                        ]),
                        Some("fn one() {}\n"),
                    )
                    .unwrap(),
                },
                BatchFileEdit {
                    canonical_path: f2.clone(),
                    display_path: f2.to_string_lossy().to_string(),
                    original: "fn two() {}\n".to_string(),
                    modified: "fn two_edited() {}\n".to_string(),
                    changes: parse_edit_changes(
                        &serde_json::json!([
                            {"type": "replace_text", "old_text": "fn two() {}", "new_text": "fn two_edited() {}"}
                        ]),
                        Some("fn two() {}\n"),
                    )
                    .unwrap(),
                },
            ],
        };

        let result = apply_batch_atomic(&plan).await;

        // Restore permissions so tempdir cleanup can succeed.
        let mut perms = std::fs::metadata(readonly_dir.path())
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(readonly_dir.path(), perms).unwrap();

        assert!(
            result.is_err(),
            "the read-only directory must fail the write"
        );
        assert_eq!(
            std::fs::read_to_string(&f1).unwrap(),
            "fn one() {}\n",
            "file 1 must be ROLLED BACK to its original content after file 2's write failed"
        );
        assert_eq!(
            std::fs::read_to_string(&f2).unwrap(),
            "fn two() {}\n",
            "file 2 must be untouched"
        );
    }

    /// Whether the process runs as root (uid 0): POSIX mode bits do not
    /// block root, so permission-based fault injection is not possible.
    fn running_as_root() -> bool {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(str::to_owned))
            .and_then(|l| {
                l.split_whitespace()
                    .nth(2)
                    .and_then(|v| v.parse::<u32>().ok())
            })
            .is_some_and(|euid| euid == 0)
    }

    /// §4.3: same-file entries under different spellings (relative vs
    /// absolute) must merge into ONE plan entry; otherwise the second write
    /// would silently overwrite the first edit while both are reported.
    #[tokio::test]
    async fn test_batch_groups_same_file_spellings_into_one_plan() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("one.rs");
        std::fs::write(&f1, "fn one() {}\nfn uno() {}\n").unwrap();
        let changes = serde_json::json!([
            {
                "file_path": "one.rs",
                "type": "replace_text",
                "old_text": "fn one() {}",
                "new_text": "fn one_renamed() {}"
            },
            {
                "file_path": f1.to_str().unwrap(),
                "type": "replace_text",
                "old_text": "fn uno() {}",
                "new_text": "fn uno_renamed() {}"
            }
        ]);
        let plan = resolve_batch(&changes, None, dir.path())
            .await
            .expect("plan resolves");
        assert_eq!(
            plan.files.len(),
            1,
            "both spellings must collapse into one file plan"
        );
        assert_eq!(
            plan.files[0].changes.len(),
            2,
            "changes merge in request order"
        );
        assert!(plan.files[0].modified.contains("fn one_renamed()"));
        assert!(plan.files[0].modified.contains("fn uno_renamed()"));
    }

    /// §4.3: replay against a cached preview with a stale or missing
    /// integrity hash is rejected before anything is planned.
    #[tokio::test]
    async fn test_resolve_batch_from_changes_hash_guard() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("one.rs");
        std::fs::write(&f1, "fn one() {}\n").unwrap();
        let changes = parse_edit_changes(
            &serde_json::json!([
                {
                    "file_path": f1.to_str().unwrap(),
                    "type": "replace_text",
                    "old_text": "fn one() {}",
                    "new_text": "fn one_edited() {}"
                }
            ]),
            None,
        )
        .unwrap();
        let key = f1.to_string_lossy().to_string();
        let hash = blake3::hash(b"fn one() {}\n").to_hex().to_string();

        // Missing hash → reject.
        let result =
            resolve_batch_from_changes(&changes, &std::collections::HashMap::new(), dir.path())
                .await;
        assert!(result.is_err(), "missing integrity hash must reject replay");

        // Stale hash (file changed since preview) → reject.
        std::fs::write(&f1, "fn one() {}\n// drifted\n").unwrap();
        let stale = std::collections::HashMap::from([(key.clone(), hash.clone())]);
        let result = resolve_batch_from_changes(&changes, &stale, dir.path()).await;
        assert!(result.is_err(), "stale integrity hash must reject replay");

        // Matching hash → plan resolves and replays onto the previewed bytes.
        std::fs::write(&f1, "fn one() {}\n").unwrap();
        let fresh = std::collections::HashMap::from([(key, hash)]);
        let plan = resolve_batch_from_changes(&changes, &fresh, dir.path())
            .await
            .expect("matching hash must replay");
        assert_eq!(plan.files.len(), 1);
        assert!(plan.files[0].modified.contains("fn one_edited() {}"));
    }

    /// A batch entry that inherits the top-level `file_path` must carry that
    /// owner on its PARSED change. The preview caches the parsed changes, and
    /// a token-only apply rebuilds the plan from them; an owner that existed
    /// only for grouping would leave `file_path: None` in the cache and the
    /// apply would reject the batch as "a change without a file_path".
    #[tokio::test]
    async fn test_batch_inherited_file_path_is_written_into_each_change() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("one.rs");
        let f2 = dir.path().join("two.rs");
        std::fs::write(&f1, "fn one() {}\n").unwrap();
        std::fs::write(&f2, "fn two() {}\n").unwrap();
        // Top-level owner is `one.rs`; the second entry names its own file.
        let changes = serde_json::json!([
            {
                "type": "replace_text",
                "old_text": "fn one() {}",
                "new_text": "fn one_edited() {}"
            },
            {
                "file_path": "two.rs",
                "type": "replace_text",
                "old_text": "fn two() {}",
                "new_text": "fn two_edited() {}"
            }
        ]);
        let plan = resolve_batch(&changes, Some("one.rs"), dir.path())
            .await
            .expect("plan resolves");
        assert_eq!(plan.files.len(), 2);
        for file in &plan.files {
            for change in &file.changes {
                let crate::edit::EditChange::ReplaceText { file_path, .. } = change else {
                    panic!("expected replace_text");
                };
                assert!(
                    file_path.is_some(),
                    "every parsed change must name its owner (display_path={})",
                    file.display_path
                );
            }
        }
        // The cached-preview replay (token-only apply) accepts exactly these
        // changes and applies both edits.
        let mut cached = Vec::new();
        let mut hashes = std::collections::HashMap::new();
        for file in &plan.files {
            cached.extend(file.changes.iter().cloned());
            hashes.insert(
                file.canonical_path.to_string_lossy().to_string(),
                blake3::hash(file.original.as_bytes()).to_hex().to_string(),
            );
        }
        let replayed = resolve_batch_from_changes(&cached, &hashes, dir.path())
            .await
            .expect("token-only apply must accept the cached changes");
        assert_eq!(replayed.files.len(), 2);
        assert!(replayed.files[0].modified.contains("fn one_edited() {}"));
        assert!(replayed.files[1].modified.contains("fn two_edited() {}"));
    }
}
