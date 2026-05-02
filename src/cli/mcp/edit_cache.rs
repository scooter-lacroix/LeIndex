use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use crate::edit::EditChange;
use serde::{Deserialize, Serialize};
use crate::cli::mcp::protocol::JsonRpcError;
use once_cell::sync::Lazy;

/// Entry in the edit cache, representing a previewed but not yet applied edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditCacheEntry {
    /// Path to the file being edited.
    pub file_path: PathBuf,
    /// Original file content before any changes.
    pub original_text: String,
    /// Modified file content after applying changes in memory.
    pub modified_text: String,
    /// The list of changes that were previewed.
    pub changes: Vec<EditChange>,
    /// When this entry was created.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A cache for edit previews, supporting both in-memory (hot) and on-disk (cold) storage.
pub struct EditCache {
    /// Hot cache in memory for fast access during a session.
    entries: Mutex<HashMap<PathBuf, EditCacheEntry>>,
}

impl EditCache {
    /// Create a new empty edit cache.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Store an edit preview in the cache.
    pub async fn set(&self, project_storage: &Path, entry: EditCacheEntry) -> Result<(), JsonRpcError> {
        let file_path = entry.file_path.clone();
        
        // Canonicalize file path for consistent keys
        let abs_path = if file_path.is_absolute() {
            file_path.clone()
        } else {
            file_path.canonicalize().unwrap_or(file_path.clone())
        };

        {
            let mut entries = self.entries.lock().await;
            entries.insert(abs_path.clone(), entry.clone());
        }
        
        // Cold storage: persist to project storage directory
        let cache_dir = project_storage.join("edit_cache");
        if !cache_dir.exists() {
            std::fs::create_dir_all(&cache_dir).map_err(|e| {
                JsonRpcError::internal_error(format!(\"Failed to create edit cache directory: {}\", e))
            })?;
        }
        
        let hash = blake3::hash(abs_path.to_string_lossy().as_bytes()).to_hex();
        let cache_file = cache_dir.join(format!(\"{}.json\", hash));
        
        let json = serde_json::to_string_pretty(&entry).map_err(|e| {
            JsonRpcError::internal_error(format!(\"Failed to serialize edit cache: {}\", e))
        })?;
        
        std::fs::write(cache_file, json).map_err(|e| {
            JsonRpcError::internal_error(format!(\"Failed to write edit cache to disk: {}\", e))
        })?;
        
        Ok(())
    }

    /// Retrieve an edit preview from the cache.
    pub async fn get(&self, project_storage: &Path, file_path: &Path) -> Option<EditCacheEntry> {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            file_path.canonicalize().unwrap_or(file_path.to_path_buf())
        };

        // Try hot cache first
        {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(&abs_path) {
                return Some(entry.clone());
            }
        }
        
        // Try cold storage fallback
        let hash = blake3::hash(abs_path.to_string_lossy().as_bytes()).to_hex();
        let cache_file = project_storage.join(\"edit_cache\").join(format!(\"{}.json\", hash));
        
        if cache_file.exists() {
            if let Ok(json) = std::fs::read_to_string(&cache_file) {
                if let Ok(entry) = serde_json::from_str::<EditCacheEntry>(&json) {
                    // Backfill hot cache
                    let mut entries = self.entries.lock().await;
                    entries.insert(abs_path, entry.clone());
                    return Some(entry);
                }
            }
        }
        
        None
    }

    /// Clear an edit preview from the cache (called after successful apply).
    pub async fn clear(&self, project_storage: &Path, file_path: &Path) {
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            file_path.canonicalize().unwrap_or(file_path.to_path_buf())
        };

        {
            let mut entries = self.entries.lock().await;
            entries.remove(&abs_path);
        }
        
        let hash = blake3::hash(abs_path.to_string_lossy().as_bytes()).to_hex();
        let cache_file = project_storage.join(\"edit_cache\").join(format!(\"{}.json\", hash));
        if cache_file.exists() {
            let _ = std::fs::remove_file(cache_file);
        }
    }
}

/// Global singleton for edit caching.
pub static GLOBAL_EDIT_CACHE: Lazy<EditCache> = Lazy::new(EditCache::new);
