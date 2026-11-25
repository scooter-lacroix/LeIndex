use crate::cli::mcp::protocol::JsonRpcError;
use crate::edit::EditChange;
use lru::LruCache;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Per-file blake3 hash of the content the preview was computed against
    /// (single-file entries key on `file_path`). Apply re-validates these so
    /// a file changed between preview and apply is rejected with a clear
    /// error instead of silently mis-merging.
    #[serde(default)]
    pub original_hashes: HashMap<String, String>,
}

impl EditCacheEntry {
    /// Build a cache entry for one file, hashing its pre-edit content.
    pub fn single_file(
        file_path: PathBuf,
        preview_token: String,
        original_text: String,
        modified_text: String,
        changes: Vec<EditChange>,
    ) -> Self {
        let hash = blake3::hash(original_text.as_bytes()).to_hex().to_string();
        Self {
            original_hashes: HashMap::from([(file_path.to_string_lossy().to_string(), hash)]),
            file_path,
            preview_token,
            original_text,
            modified_text,
            changes,
            timestamp: chrono::Utc::now(),
        }
    }

    /// True when every file this entry touched is still byte-identical to
    /// what the preview was computed against. An empty map (entries written
    /// before hash tracking existed) is treated as valid — the expected-
    /// content atomic write remains the hard safety net.
    pub fn files_unchanged_since_preview(&self) -> bool {
        self.original_hashes.iter().all(|(path, hash)| {
            std::fs::read(path)
                .map(|bytes| blake3::hash(&bytes).to_hex().to_string() == *hash)
                .unwrap_or(false)
        })
    }

    /// The filesystem path this entry edits. Token-alias entries store the
    /// synthetic `__token__:` cache key in `file_path`; the real path lives
    /// in `original_hashes` (single-file entries carry exactly one).
    pub fn real_file_path(&self) -> &Path {
        self.original_hashes
            .keys()
            .next()
            .map_or(self.file_path.as_path(), Path::new)
    }

    /// Estimated byte size of this entry in the hot cache.
    pub fn estimated_size(&self) -> usize {
        let path_bytes = self.file_path.as_os_str().len();
        // On Windows, OsStr::len() returns u16 code units, not bytes
        #[cfg(target_os = "windows")]
        let path_bytes = path_bytes * 2;
        path_bytes
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
            entries: Mutex::new((LruCache::new(NonZeroUsize::new(10_000).unwrap()), 0)),
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
        if let Ok(json) = tokio::fs::read_to_string(&cache_file).await
            && let Ok(entry) = serde_json::from_str::<EditCacheEntry>(&json)
        {
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

    /// Store a preview under its file path AND a token alias so
    /// `apply` can resolve `preview_token` alone (§3.4 advertised
    /// path). The alias key is scoped to `project_storage` so a token
    /// can never resolve a preview belonging to a different project,
    /// and the alias shares the primary entry's payload — its bytes are
    /// NOT counted twice in the hot-cache budget (the payload is the same).
    /// An alias storage failure propagates; the primary entry is durable
    /// regardless.
    pub async fn set_with_token_alias(
        &self,
        project_storage: &Path,
        entry: EditCacheEntry,
    ) -> Result<Result<(), EntryTooLargeError>, JsonRpcError> {
        let primary = self.set(project_storage, entry.clone()).await?;
        let alias = EditCacheEntry {
            file_path: token_alias_path(project_storage, &entry.preview_token),
            ..entry
        };
        self.set_alias(project_storage, alias).await?;
        Ok(primary)
    }

    /// Store a token-alias entry WITHOUT the pre-charge shortcut.
    ///
    /// The alias is a deep clone of the primary entry — its own `String`
    /// copies of the original and modified text and its own `changes` — so it
    /// occupies real memory of its own. It must therefore be charged against
    /// the hot-cache budget and be evictable like any other entry (Codex
    /// round 7, id 4690978432). Leaving it uncharged let a long series of
    /// previews retain N x entry_size while `total_bytes` only ever counted
    /// the primaries, so the advertised 8 MiB cap did not bound memory.
    async fn set_alias(
        &self,
        project_storage: &Path,
        entry: EditCacheEntry,
    ) -> Result<(), JsonRpcError> {
        let entry_size = entry.estimated_size();
        if entry_size > EDIT_CACHE_MAX_ENTRY_BYTES {
            return Ok(());
        }

        let (abs_path, cache_file) = self
            .get_abs_path_and_cache_file(project_storage, &entry.file_path)
            .await;

        let cache_dir = cache_file.parent().unwrap();
        if tokio::fs::metadata(cache_dir).await.is_err() {
            tokio::fs::create_dir_all(cache_dir).await.map_err(|e| {
                JsonRpcError::internal_error(format!("Failed to create edit cache directory: {e}"))
            })?;
        }
        let json = serde_json::to_string_pretty(&entry).map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to serialize edit cache: {e}"))
        })?;
        tokio::fs::write(&cache_file, json).await.map_err(|e| {
            JsonRpcError::internal_error(format!("Failed to write edit cache to disk: {e}"))
        })?;

        {
            let mut guard = self.entries.lock().await;
            let (ref mut entries, ref mut total_bytes) = *guard;
            // A stale alias for the same token (e.g. stored before this
            // change) may still hold budget bytes — release them, then charge
            // this alias like any other entry so it is bounded and evictable.
            if let Some(existing) = entries.pop(&abs_path) {
                *total_bytes = total_bytes.saturating_sub(existing.estimated_size());
            }
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
        Ok(())
    }

    /// Resolve a preview by its token alone (no file_path needed). The
    /// alias key is scoped to `project_storage` — a token can never
    /// resolve a preview cached under a different project.
    pub async fn get_by_token(
        &self,
        project_storage: &Path,
        token: &str,
    ) -> Option<EditCacheEntry> {
        self.get(project_storage, &token_alias_path(project_storage, token))
            .await
    }

    /// Clear both the file-path entry and the token alias after an apply —
    /// but ONLY when the cached entry under `file_path` still belongs to
    /// `token`. A newer preview for the same file (different token) must
    /// not be destroyed by the apply of an older one.
    pub async fn clear_with_token_alias(
        &self,
        project_storage: &Path,
        file_path: &Path,
        token: &str,
    ) {
        if let Some(existing) = self.get(project_storage, file_path).await
            && existing.preview_token != token
        {
            return;
        }
        self.clear(project_storage, file_path).await;
        self.clear(project_storage, &token_alias_path(project_storage, token))
            .await;
    }
}

/// Synthetic cache key for a token alias, scoped to the project storage so
/// the same token can never collide across projects. Real paths cannot
/// contain the NUL-free `__token__:` prefix in normal use, and the value is
/// hashed for cold storage.
fn token_alias_path(project_storage: &Path, token: &str) -> PathBuf {
    let storage_fp = blake3::hash(project_storage.to_string_lossy().as_bytes()).to_hex();
    PathBuf::from(format!("__token__:{storage_fp}:{token}"))
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
                file_path: None,
                start: 0,
                end: original_len.min(1),
                new_text: "y".to_string(),
            }],
            timestamp: chrono::Utc::now(),
            original_hashes: HashMap::new(),
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

    /// §3.4: apply-by-token re-validates the file is unchanged since the
    /// preview. `single_file` must hash the PRE-edit content, and the hash
    /// must stop matching once the file changes on disk.
    #[test]
    fn test_single_file_entry_rejects_file_changed_since_preview() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("stale_check.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        let entry = EditCacheEntry::single_file(
            file.clone(),
            "tok-stale".to_string(),
            "fn a() {}\n".to_string(),
            "fn b() {}\n".to_string(),
            vec![],
        );
        assert!(
            entry.files_unchanged_since_preview(),
            "untouched file must pass the staleness check"
        );

        std::fs::write(&file, "fn a() { changed }\n").unwrap();
        assert!(
            !entry.files_unchanged_since_preview(),
            "a file modified between preview and apply must FAIL the staleness check"
        );
    }

    /// §3.4: a preview stored under its file path must also resolve from
    /// the token alone (the advertised `preview_token + project_path`
    /// apply path).
    #[tokio::test]
    async fn test_token_alias_resolves_without_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        let file = dir.path().join("aliased.rs");
        std::fs::write(&file, "fn aliased() {}\n").unwrap();
        let cache = EditCache::new();
        let entry = EditCacheEntry::single_file(
            file,
            "tok-alias-123".to_string(),
            "fn aliased() {}\n".to_string(),
            "fn renamed() {}\n".to_string(),
            vec![],
        );
        cache
            .set_with_token_alias(&storage, entry)
            .await
            .expect("no IO error")
            .expect("within size limit");

        let resolved = cache
            .get_by_token(&storage, "tok-alias-123")
            .await
            .expect("token-only lookup must resolve the preview");
        assert_eq!(resolved.preview_token, "tok-alias-123");
        // The alias entry keys the cache on the synthetic `__token__:` path;
        // the REAL file must come through `real_file_path()` so a token-only
        // apply can resolve which file to write.
        assert!(
            resolved.real_file_path().ends_with("aliased.rs"),
            "real_file_path must carry the actual file, got: {}",
            resolved.real_file_path().display()
        );
        assert_eq!(resolved.original_text, "fn aliased() {}\n");
    }

    /// §3.4: the token alias is scoped to its project storage — the same
    /// token under a different storage must NOT resolve the preview.
    #[tokio::test]
    async fn test_token_alias_is_scoped_to_project_storage() {
        let dir = tempfile::tempdir().unwrap();
        let storage_a = dir.path().join(".leindex-a");
        let storage_b = dir.path().join(".leindex-b");
        let file = dir.path().join("aliased.rs");
        std::fs::write(&file, "fn aliased() {}\n").unwrap();
        let cache = EditCache::new();
        let entry = EditCacheEntry::single_file(
            file,
            "tok-scoped-1".to_string(),
            "fn aliased() {}\n".to_string(),
            "fn renamed() {}\n".to_string(),
            vec![],
        );
        cache
            .set_with_token_alias(&storage_a, entry)
            .await
            .expect("no IO error")
            .expect("within size limit");

        assert!(
            cache
                .get_by_token(&storage_a, "tok-scoped-1")
                .await
                .is_some(),
            "token must resolve within its own storage"
        );
        assert!(
            cache
                .get_by_token(&storage_b, "tok-scoped-1")
                .await
                .is_none(),
            "same token must NOT resolve from a different storage"
        );
    }

    /// §3.4: clear_with_token_alias must NOT destroy a newer preview under
    /// the same file when its token differs.
    #[tokio::test]
    async fn test_clear_with_token_alias_keeps_newer_preview() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join(".leindex");
        let file = dir.path().join("aliased.rs");
        std::fs::write(&file, "fn aliased() {}\n").unwrap();
        let cache = EditCache::new();

        let newer = EditCacheEntry::single_file(
            file.clone(),
            "tok-newer".to_string(),
            "fn aliased() {}\n".to_string(),
            "fn newer_edit() {}\n".to_string(),
            vec![],
        );
        cache
            .set_with_token_alias(&storage, newer)
            .await
            .expect("no IO error")
            .expect("within size limit");

        // Applying an OLDER token (already not in the cache) must not
        // clear the newer preview.
        cache
            .clear_with_token_alias(&storage, &file, "tok-older")
            .await;
        let after = cache.get(&storage, &file).await;
        assert!(
            after.is_some() && after.unwrap().preview_token == "tok-newer",
            "newer preview must survive a different token's clear"
        );

        // But the owning token's clear removes both the primary and alias.
        cache
            .clear_with_token_alias(&storage, &file, "tok-newer")
            .await;
        assert!(
            cache.get(&storage, &file).await.is_none(),
            "primary must clear"
        );
        assert!(
            cache.get_by_token(&storage, "tok-newer").await.is_none(),
            "alias must clear"
        );
    }

    /// Codex round 7 (id 4690978432): the token alias is a DEEP CLONE of the
    /// entry (its own original/modified text and changes), yet `set_alias`
    /// stored it without charging `total_bytes`. A long series of previews
    /// therefore retained N x entry_size while the budget only ever saw the
    /// primaries, so the 8 MiB cap bounded nothing. Every alias must be
    /// charged, and must therefore be evictable.
    #[tokio::test]
    async fn test_token_aliases_are_charged_against_the_hot_cache_budget() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_path_buf();
        let cache = EditCache::new();
        let file = storage.join("f.rs");
        std::fs::write(&file, "fn f() {}\n").unwrap();

        let mut entry = EditCacheEntry::single_file(
            file.clone(),
            "tok-alias-budget".to_string(),
            "x".repeat(64 * 1024),
            "y".repeat(64 * 1024),
            Vec::new(),
        );
        entry.original_hashes =
            HashMap::from([(file.to_string_lossy().to_string(), "h".to_string())]);
        let entry_size = entry.estimated_size();
        cache
            .set_with_token_alias(&storage, entry)
            .await
            .unwrap()
            .unwrap();

        let hot = cache.hot_cache_bytes().await;
        assert!(
            hot >= entry_size * 2,
            "the alias must be charged too: hot={hot}, entry={entry_size} \
             (one entry means the alias was free)"
        );
        assert!(
            hot <= EDIT_CACHE_TOTAL_CAP_BYTES,
            "the budget must still hold: hot={hot}"
        );
    }
}
