use crate::cli::mcp::protocol::JsonRpcError;
use crate::edit::EditChange;
use lru::LruCache;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;

// ============================================================================
// A+ Edit-preview cache budget constants (Section 8.2)
// ============================================================================

/// Maximum bytes for a single edit-preview entry.
/// Entries larger than this are rejected to avoid inflating hot resident state.
pub const EDIT_CACHE_MAX_ENTRY_BYTES: usize = 256 * 1024; // 256 KiB

/// Maximum total bytes for the hot edit-preview cache.
/// Inserts that would exceed this cap trigger LRU eviction.
pub const EDIT_CACHE_TOTAL_CAP_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

/// Entry in the edit cache, representing a previewed but not yet applied edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditCacheEntry {
    /// Path to the file being edited.
    pub file_path: PathBuf,
    /// A unique token for this preview request to prevent race conditions or cross-client application.
    pub preview_token: String,
    /// Original file content before any changes.
    pub original_text: String,
    /// Modified file content after applying changes in memory.
    pub modified_text: String,
    /// The list of changes that were previewed.
    pub changes: Vec<EditChange>,
    /// When this entry was created.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EditCacheEntry {
    /// Estimated byte size of this entry in the hot cache.
    pub fn estimated_size(&self) -> usize {
        self.file_path.as_os_str().len()
            + self.preview_token.len()
            + self.original_text.len()
            + self.modified_text.len()
            + self
                .changes
                .iter()
                .map(|c| c.estimated_size())
                .sum::<usize>()
            + 64 // overhead estimate for timestamp, metadata
    }
}

/// Error returned when an edit-preview entry exceeds the per-entry size limit.
#[derive(Debug)]
pub struct EntryTooLargeError {
    /// Actual size of the entry in bytes.
    pub size: usize,
    /// Configured per-entry limit in bytes.
    pub limit: usize,
}

impl std::fmt::Display for EntryTooLargeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "edit-preview entry ({} bytes) exceeds per-entry limit ({} bytes)",
            self.size, self.limit
        )
    }
}

impl std::error::Error for EntryTooLargeError {}

/// A cache for edit previews, supporting both in-memory (hot) and on-disk (cold) storage.
/// The hot cache is bounded by both per-entry and total-byte caps.
pub struct EditCache {
    /// Hot cache in memory for fast access during a session.
    /// `total_bytes` lives inside the Mutex alongside the map so all mutations
    /// are naturally synchronized without needing AtomicUsize.
    /// Uses LruCache for O(1) eviction instead of O(N) min_by_key scan.
    entries: Mutex<(LruCache<PathBuf, EditCacheEntry>, usize)>,
}

impl Default for EditCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new((
                LruCache::new(NonZeroUsize::new(10_000).unwrap()),
                0,
            )),
        }
    }
}

impl EditCache {
    /// Create a new empty edit cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Internal helper to resolve absolute path and cache file location.
    async fn get_abs_path_and_cache_file(
        &self,
        project_storage: &Path,
        file_path: &Path,
    ) -> (PathBuf, PathBuf) {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            tokio::fs::canonicalize(file_path)
                .await
                .unwrap_or_else(|_| file_path.to_path_buf())
        };

        let hash = blake3::hash(abs_path.to_string_lossy().as_bytes()).to_hex();
        let cache_file = project_storage
            .join("edit_cache")
            .join(format!("{}.json", hash));

        (abs_path, cache_file)
    }

    /// Store an edit preview in the cache.
    ///
    /// Returns `Ok(Err(EntryTooLargeError))` if the entry exceeds the per-entry
    /// byte limit. Returns `Err(JsonRpcError)` for I/O failures.
    pub async fn set(
        &self,
        project_storage: &Path,
        entry: EditCacheEntry,
    ) -> Result<Result<(), EntryTooLargeError>, JsonRpcError> {
        let entry_size = entry.estimated_size();

        // Reject oversized entries synchronously (VAL-APLUS-012)
        if entry_size > EDIT_CACHE_MAX_ENTRY_BYTES {
            return Ok(Err(EntryTooLargeError {
                size: entry_size,
                limit: EDIT_CACHE_MAX_ENTRY_BYTES,
            }));
        }

        let (abs_path, cache_file) = self
            .get_abs_path_and_cache_file(project_storage, &entry.file_path)
            .await;

        // Cold storage: persist to project storage directory
        let cache_dir = cache_file.parent().unwrap();
        if tokio::fs::metadata(cache_dir).await.is_err() {
            tokio::fs::create_dir_all(cache_dir).await.map_err(|e| {
                JsonRpcError::internal_error(format!(
                    "Failed to create edit cache directory: {}",
                    e
                ))
            })?;
        }

        let json = serde_json::to_string_pretty(&entry).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to serialize edit cache: {}", e))
        })?;

        tokio::fs::write(&cache_file, json).await.map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write edit cache to disk: {}", e))
        })?;

        // Update hot cache only AFTER successful disk persistence.
        // Enforce total-byte cap via synchronous eviction (VAL-APLUS-013).
        {
            let mut guard = self.entries.lock().await;
            let (ref mut entries, ref mut total_bytes) = *guard;

            // Remove existing entry first to prevent double-subtraction during eviction.
            // If we only subtract its size but leave it in the map, the eviction loop
            // could remove it again and subtract its size a second time (underflow).
            if let Some(existing) = entries.pop(&abs_path) {
                *total_bytes = total_bytes.saturating_sub(existing.estimated_size());
            }

            // Evict until there is room using O(1) LRU eviction
            while *total_bytes + entry_size > EDIT_CACHE_TOTAL_CAP_BYTES {
                if let Some((_, removed)) = entries.pop_lru() {
                    *total_bytes = total_bytes.saturating_sub(removed.estimated_size());
                } else {
                    break;
                }
            }

            entries.put(abs_path, entry);
            *total_bytes = total_bytes.saturating_add(entry_size);
        }

        Ok(Ok(()))
    }

    /// Retrieve an edit preview from the cache.
    pub async fn get(&self, project_storage: &Path, file_path: &Path) -> Option<EditCacheEntry> {
        let (abs_path, cache_file) = self
            .get_abs_path_and_cache_file(project_storage, file_path)
            .await;

        // Try hot cache first
        {
            let mut guard = self.entries.lock().await;
            if let Some(entry) = guard.0.get_mut(&abs_path) {
                return Some(entry.clone());
            }
        }

        // Try cold storage fallback
        if let Ok(json) = tokio::fs::read_to_string(&cache_file).await {
            if let Ok(entry) = serde_json::from_str::<EditCacheEntry>(&json) {
                // Backfill hot cache only if within budget
                let entry_size = entry.estimated_size();
                if entry_size <= EDIT_CACHE_MAX_ENTRY_BYTES {
                    let mut guard = self.entries.lock().await;
                    let (ref mut entries, ref mut total_bytes) = *guard;
                    if let Some(existing) = entries.pop(&abs_path) {
                        *total_bytes = total_bytes.saturating_sub(existing.estimated_size());
                    }
                    // Evict if needed using O(1) LRU eviction
                    while *total_bytes + entry_size > EDIT_CACHE_TOTAL_CAP_BYTES {
                        if let Some((_, removed)) = entries.pop_lru() {
                            *total_bytes = total_bytes.saturating_sub(removed.estimated_size());
                        } else {
                            break;
                        }
                    }
                    *total_bytes = total_bytes.saturating_add(entry_size);
                    entries.put(abs_path, entry.clone());
                }
                return Some(entry);
            }
        }

        None
    }

    /// Clear an edit preview from the cache (called after successful apply).
    pub async fn clear(&self, project_storage: &Path, file_path: &Path) {
        let (abs_path, cache_file) = self
            .get_abs_path_and_cache_file(project_storage, file_path)
            .await;

        {
            let mut guard = self.entries.lock().await;
            if let Some(removed) = guard.0.pop(&abs_path) {
                guard.1 = guard.1.saturating_sub(removed.estimated_size());
            }
        }

        let _ = tokio::fs::remove_file(cache_file).await;
    }

    /// Current total bytes in the hot cache (for testing/telemetry).
    pub async fn hot_cache_bytes(&self) -> usize {
        self.entries.lock().await.1
    }
}

/// Global singleton for edit caching.
pub static GLOBAL_EDIT_CACHE: Lazy<EditCache> = Lazy::new(EditCache::new);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditChange;
    use std::path::PathBuf;

    fn make_entry(original_len: usize, modified_len: usize) -> EditCacheEntry {
        EditCacheEntry {
            file_path: PathBuf::from("/test/file.rs"),
            preview_token: "test-token".to_string(),
            original_text: "x".repeat(original_len),
            modified_text: "y".repeat(modified_len),
            changes: vec![EditChange::ReplaceText {
                start: 0,
                end: original_len.min(1),
                new_text: "y".to_string(),
            }],
            timestamp: chrono::Utc::now(),
        }
    }

    // A+ VAL-APLUS-012: MCP edit preview cache rejects oversized entries
    #[tokio::test]
    async fn test_edit_cache_rejects_oversized_entry() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cache = EditCache::new();

        // Create an entry larger than EDIT_CACHE_MAX_ENTRY_BYTES (256 KiB)
        let oversized = make_entry(EDIT_CACHE_MAX_ENTRY_BYTES + 1000, 10);
        let result = cache
            .set(temp_dir.path(), oversized)
            .await
            .expect("no IO error");

        assert!(
            result.is_err(),
            "oversized entry should be rejected, but was accepted"
        );
        let err = result.unwrap_err();
        assert_eq!(err.limit, EDIT_CACHE_MAX_ENTRY_BYTES);
        assert!(err.size > EDIT_CACHE_MAX_ENTRY_BYTES);
    }

    // A+ VAL-APLUS-013: MCP edit preview cache total residency is capped
    #[tokio::test]
    async fn test_edit_cache_total_residency_capped() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cache = EditCache::new();

        // Insert many small entries that together exceed the total cap
        let entry_size = 100_000; // ~100 KiB each
        let entries_to_fill = (EDIT_CACHE_TOTAL_CAP_BYTES / entry_size) + 3;

        for i in 0..entries_to_fill {
            let mut entry = make_entry(entry_size / 2, entry_size / 2);
            entry.file_path = PathBuf::from(format!("/test/file_{}.rs", i));
            let result = cache
                .set(temp_dir.path(), entry)
                .await
                .expect("no IO error");
            assert!(result.is_ok(), "entry {} should be accepted", i);
        }

        // Total hot cache bytes should not exceed the cap
        assert!(
            cache.hot_cache_bytes().await <= EDIT_CACHE_TOTAL_CAP_BYTES + 10_000,
            "hot cache bytes ({}) should not exceed cap ({})",
            cache.hot_cache_bytes().await,
            EDIT_CACHE_TOTAL_CAP_BYTES
        );
    }

    #[tokio::test]
    async fn test_edit_cache_backfill_replaces_existing_hot_entry_bytes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cache = EditCache::new();

        let mut initial = make_entry(32, 32);
        initial.file_path = PathBuf::from("/test/backfill.rs");
        cache
            .set(temp_dir.path(), initial.clone())
            .await
            .unwrap()
            .unwrap();
        {
            let mut guard = cache.entries.lock().await;
            let mut disk_entry = make_entry(160, 160);
            disk_entry.file_path = initial.file_path.clone();
            let disk_entry_size = disk_entry.estimated_size();
            guard.1 -= initial.estimated_size();
            guard.0.put(initial.file_path.clone(), disk_entry.clone());
            guard.1 += disk_entry_size;
        }

        let backed_fill = cache
            .get(temp_dir.path(), &initial.file_path)
            .await
            .expect("entry should be recovered from cold storage");

        assert_eq!(backed_fill.file_path, initial.file_path);
        let expected = backed_fill.estimated_size();
        assert_eq!(
            cache.hot_cache_bytes().await,
            expected,
            "backfill should replace the existing hot entry bytes exactly"
        );
    }

    #[test]
    fn test_edit_cache_entry_estimated_size() {
        let entry = make_entry(100, 200);
        let size = entry.estimated_size();
        assert!(size > 300, "estimated size should account for text content");
    }
}
