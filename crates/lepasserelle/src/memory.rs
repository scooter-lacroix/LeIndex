// Memory-aware resource management with cache spilling
//
// Implements automatic cache spilling to disk when memory thresholds are exceeded,
// using LRU eviction policy and efficient binary serialization.

use bincode::{config, deserialize, serialize};
use lru::LruCache;
use psutil::{process, process::Process};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{debug, info, warn};

// ============================================================================
// CONFIGURATION
// ============================================================================

/// Memory manager configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// RSS threshold for spilling (0.0-1.0)
    pub spill_threshold: f64,

    /// Check interval in seconds
    pub check_interval_secs: u64,

    /// Whether to enable automatic spilling
    pub auto_spill: bool,

    /// Maximum cache size in bytes (0 = unlimited)
    pub max_cache_bytes: usize,

    /// Cache directory path
    pub cache_dir: PathBuf,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            spill_threshold: 0.85,
            check_interval_secs: 30,
            auto_spill: true,
            max_cache_bytes: 500_000_000, // 500MB default
            cache_dir: PathBuf::from(".leindex/cache"),
        }
    }
}

// ============================================================================
// CACHE ENTRY TYPES
// ============================================================================

/// Cache entry identifier
pub type CacheKey = String;

/// Different types of cacheable items
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEntry {
    /// Program Dependence Graph
    PDG {
        project_id: String,
        node_count: usize,
        edge_count: usize,
        serialized_data: Vec<u8>,
    },

    /// Search index entries
    SearchIndex {
        project_id: String,
        entry_count: usize,
        serialized_data: Vec<u8>,
    },

    /// Analysis results
    Analysis {
        query: String,
        timestamp: u64,
        serialized_data: Vec<u8>,
    },

    /// Generic binary data
    Binary {
        metadata: HashMap<String, String>,
        serialized_data: Vec<u8>,
    },
}

impl CacheEntry {
    /// Get the size in bytes of this cache entry
    pub fn size_bytes(&self) -> usize {
        match self {
            CacheEntry::PDG { serialized_data, .. } => serialized_data.len(),
            CacheEntry::SearchIndex { serialized_data, .. } => serialized_data.len(),
            CacheEntry::Analysis { serialized_data, .. } => serialized_data.len(),
            CacheEntry::Binary { serialized_data, .. } => serialized_data.len(),
        }
    }

    /// Get a description of this entry
    pub fn description(&self) -> String {
        match self {
            CacheEntry::PDG { project_id, node_count, .. } => {
                format!("PDG for {} ({} nodes)", project_id, node_count)
            }
            CacheEntry::SearchIndex { project_id, entry_count, .. } => {
                format!("Search index for {} ({} entries)", project_id, entry_count)
            }
            CacheEntry::Analysis { query, .. } => {
                format!("Analysis for '{}'", query)
            }
            CacheEntry::Binary { metadata, .. } => {
                format!("Binary data ({})", metadata.get("type").unwrap_or(&"unknown".to_string()))
            }
        }
    }
}

// ============================================================================
// CACHE STORE
// ============================================================================

/// Cache store with LRU eviction policy
pub struct CacheStore {
    /// LRU cache for in-memory entries
    cache: LruCache<CacheKey, CacheEntry>,

    /// Total size in bytes of cached items
    total_bytes: usize,

    /// Maximum size in bytes
    max_bytes: usize,

    /// Cache directory for spilled items
    cache_dir: PathBuf,
}

impl CacheStore {
    /// Create a new cache store
    pub fn new(config: &MemoryConfig) -> Self {
        // Create cache directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&config.cache_dir) {
            warn!("Failed to create cache directory {:?}: {}", config.cache_dir, e);
        }

        // LRU cache capacity is based on number of items, not bytes
        // We'll track bytes separately
        let item_capacity = NonZeroUsize::new(100).unwrap(); // Max number of items

        Self {
            cache: LruCache::new(item_capacity),
            total_bytes: 0,
            max_bytes: config.max_cache_bytes,
            cache_dir: config.cache_dir.clone(),
        }
    }

    /// Insert an entry into the cache
    pub fn insert(&mut self, key: CacheKey, entry: CacheEntry) -> Result<(), Error> {
        let entry_size = entry.size_bytes();

        // Check if we need to evict entries
        while self.total_bytes + entry_size > self.max_bytes && !self.cache.is_empty() {
            if let Some((evicted_key, evicted_entry)) = self.cache.pop_lru() {
                let evicted_size = evicted_entry.size_bytes();
                self.total_bytes = self.total_bytes.saturating_sub(evicted_size);

                debug!("Evicted cache entry '{}' ({} bytes)", evicted_key, evicted_size);

                // Spill to disk if not auto-spill only
                if let Err(e) = self.spill_to_disk(&evicted_key, &evicted_entry) {
                    warn!("Failed to spill evicted entry to disk: {}", e);
                }
            }
        }

        // Update total bytes (account for replacing existing entry)
        if let Some(existing) = self.cache.get(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(existing.size_bytes());
        }
        self.total_bytes += entry_size;

        self.cache.put(key, entry);
        Ok(())
    }

    /// Get an entry from the cache
    pub fn get(&mut self, key: &str) -> Option<CacheEntry> {
        self.cache.get(key).cloned()
    }

    /// Remove an entry from the cache
    pub fn remove(&mut self, key: &str) -> Option<CacheEntry> {
        if let Some(entry) = self.cache.pop(key) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.size_bytes());
            Some(entry)
        } else {
            None
        }
    }

    /// Get the number of cached entries
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Get total bytes used by cache
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Clear all entries from the cache
    pub fn clear(&mut self) -> Result<usize, Error> {
        let bytes_freed = self.total_bytes;

        // Spill all entries to disk before clearing
        for (key, entry) in self.cache.iter() {
            if let Err(e) = self.spill_to_disk(key, entry) {
                warn!("Failed to spill entry '{}' to disk: {}", key, e);
            }
        }

        self.cache.clear();
        self.total_bytes = 0;

        Ok(bytes_freed)
    }

    /// Spill a specific entry to disk
    fn spill_to_disk(&self, key: &str, entry: &CacheEntry) -> Result<(), Error> {
        let cache_file = self.cache_dir.join(format!("{}.bin", sanitize_key(key)));

        // Serialize the entry
        let serialized = serialize(entry)
            .map_err(|e| Error::SpillFailed(format!("Serialization failed: {}", e)))?;

        // Write to temporary file first
        let temp_file = cache_file.with_extension("tmp");
        let mut file = std::fs::File::create(&temp_file)
            .map_err(|e| Error::SpillFailed(format!("Failed to create temp file: {}", e)))?;

        file.write_all(&serialized)
            .map_err(|e| Error::SpillFailed(format!("Failed to write cache file: {}", e)))?;

        file.sync_all()
            .map_err(|e| Error::SpillFailed(format!("Failed to sync cache file: {}", e)))?;

        // Atomic rename
        std::fs::rename(&temp_file, &cache_file)
            .map_err(|e| Error::SpillFailed(format!("Failed to rename cache file: {}", e)))?;

        debug!("Spilled cache entry '{}' to disk ({} bytes)", key, serialized.len());

        Ok(())
    }

    /// Load a spilled entry from disk
    pub fn load_from_disk(&self, key: &str) -> Result<CacheEntry, Error> {
        let cache_file = self.cache_dir.join(format!("{}.bin", sanitize_key(key)));

        if !cache_file.exists() {
            return Err(Error::CacheNotFound(format!("Cache entry '{}' not found on disk", key)));
        }

        let mut file = std::fs::File::open(&cache_file)
            .map_err(|e| Error::SpillFailed(format!("Failed to open cache file: {}", e)))?;

        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| Error::SpillFailed(format!("Failed to read cache file: {}", e)))?;

        let entry: CacheEntry = deserialize(&buffer)
            .map_err(|e| Error::SpillFailed(format!("Deserialization failed: {}", e)))?;

        debug!("Loaded cache entry '{}' from disk ({} bytes)", key, buffer.len());

        Ok(entry)
    }

    /// List all spilled cache entries on disk
    pub fn list_spilled(&self) -> Result<Vec<String>, Error> {
        let mut entries = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(entries);
        }

        for entry in std::fs::read_dir(&self.cache_dir)
            .map_err(|e| Error::SpillFailed(format!("Failed to read cache directory: {}", e)))?
        {
            let entry = entry.map_err(|e| Error::SpillFailed(format!("Failed to read dir entry: {}", e)))?;

            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".bin") {
                    let key = name.strip_suffix(".bin").unwrap_or(name);
                    entries.push(key.to_string());
                }
            }
        }

        Ok(entries)
    }

    /// Delete a spilled entry from disk
    pub fn delete_spilled(&self, key: &str) -> Result<(), Error> {
        let cache_file = self.cache_dir.join(format!("{}.bin", sanitize_key(key)));

        if cache_file.exists() {
            std::fs::remove_file(&cache_file)
                .map_err(|e| Error::SpillFailed(format!("Failed to delete cache file: {}", e)))?;

            debug!("Deleted spilled cache entry '{}'", key);
        }

        Ok(())
    }

    /// Get size of spilled cache on disk
    pub fn spilled_size_bytes(&self) -> Result<usize, Error> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }

        let mut total = 0;

        for entry in std::fs::read_dir(&self.cache_dir)
            .map_err(|e| Error::SpillFailed(format!("Failed to read cache directory: {}", e)))?
        {
            let entry = entry.map_err(|e| Error::SpillFailed(format!("Failed to read dir entry: {}", e)))?;
            total += entry.metadata()
                .map(|m| m.len() as usize)
                .unwrap_or(0);
        }

        Ok(total)
    }
}

/// Sanitize a cache key for use as a filename
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

// ============================================================================
// CACHE SPILLER
// ============================================================================

/// Handles cache spilling operations
pub struct CacheSpiller {
    /// Cache store
    store: CacheStore,

    /// Memory manager for checking thresholds
    memory_manager: MemoryManager,
}

impl CacheSpiller {
    /// Create a new cache spiller
    pub fn new(config: MemoryConfig) -> Result<Self, Error> {
        let store = CacheStore::new(&config);
        let memory_manager = MemoryManager::new(config.clone())?;

        Ok(Self {
            store,
            memory_manager,
        })
    }

    /// Get mutable reference to the cache store
    pub fn store_mut(&mut self) -> &mut CacheStore {
        &mut self.store
    }

    /// Get reference to the cache store
    pub fn store(&self) -> &CacheStore {
        &self.store
    }

    /// Check memory and spill if necessary
    pub fn check_and_spill(&mut self) -> Result<SpillResult, Error> {
        if !self.memory_manager.is_threshold_exceeded()? {
            return Ok(SpillResult {
                memory_freed: 0,
                caches_cleared: Vec::new(),
                entries_spilled: 0,
            });
        }

        info!("Memory threshold exceeded, spilling cache...");

        let rss_before = self.memory_manager.get_rss_bytes()?;
        let entries_before = self.store.len();

        // Clear a portion of the cache (20% of entries)
        let target_entries = (entries_before as f64 * 0.2).ceil() as usize;
        let mut spilled_keys = Vec::new();

        for _ in 0..target_entries {
            if let Some((key, _entry)) = self.store.cache.pop_lru() {
                spilled_keys.push(key.clone());
            }
        }

        let bytes_freed = rss_before.saturating_sub(self.memory_manager.get_rss_bytes()?);

        info!(
            "Spilled {} cache entries, freed {} bytes",
            spilled_keys.len(),
            bytes_freed
        );

        Ok(SpillResult {
            memory_freed: bytes_freed,
            caches_cleared: vec!["lru_cache".to_string()],
            entries_spilled: spilled_keys.len(),
        })
    }

    /// Spill all cache entries to disk
    pub fn spill_all(&mut self) -> Result<SpillResult, Error> {
        let rss_before = self.memory_manager.get_rss_bytes()?;
        let entries_before = self.store.len();

        let bytes_freed = self.store.clear()?;

        let rss_after = self.memory_manager.get_rss_bytes()?;

        Ok(SpillResult {
            memory_freed: rss_before.saturating_sub(rss_after),
            caches_cleared: vec!["lru_cache".to_string()],
            entries_spilled: entries_before,
        })
    }

    /// Get memory usage statistics
    pub fn memory_stats(&self) -> Result<MemoryStats, Error> {
        Ok(MemoryStats {
            rss_bytes: self.memory_manager.get_rss_bytes()?,
            total_bytes: self.memory_manager.get_total_memory()?,
            cache_entries: self.store.len(),
            cache_bytes: self.store.total_bytes(),
            spilled_entries: self.store.list_spilled()?.len(),
            spilled_bytes: self.store.spilled_size_bytes()?,
        })
    }

    /// Check if memory threshold is exceeded
    pub fn is_threshold_exceeded(&self) -> Result<bool, Error> {
        self.memory_manager.is_threshold_exceeded()
    }

    /// Get reference to the memory manager
    pub fn memory_manager(&self) -> &MemoryManager {
        &self.memory_manager
    }
}

// ============================================================================
// MEMORY MANAGER
// ============================================================================

/// Memory manager for resource-aware operations
pub struct MemoryManager {
    config: MemoryConfig,
    current_process: Process,
}

impl MemoryManager {
    /// Create a new memory manager
    pub fn new(config: MemoryConfig) -> Result<Self, Error> {
        let current_process = Process::current()
            .map_err(|e| Error::ProcessAccess(e.to_string()))?;

        Ok(Self {
            config,
            current_process,
        })
    }

    /// Get current RSS memory in bytes
    pub fn get_rss_bytes(&self) -> Result<usize, Error> {
        self.current_process
            .memory_info()
            .map(|info| info.rss() as usize)
            .map_err(|e| Error::MemoryInfo(e.to_string()))
    }

    /// Get total system memory in bytes
    pub fn get_total_memory(&self) -> Result<usize, Error> {
        psutil::memory::virtual_memory()
            .map(|mem| mem.total() as usize)
            .map_err(|e| Error::MemoryInfo(e.to_string()))
    }

    /// Check if RSS threshold is exceeded
    pub fn is_threshold_exceeded(&self) -> Result<bool, Error> {
        let rss = self.get_rss_bytes()?;
        let total = self.get_total_memory()?;
        let ratio = rss as f64 / total as f64;

        Ok(ratio > self.config.spill_threshold)
    }

    /// Get the current config
    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }
}

impl Default for MemoryManager {
    fn default() -> Self {
        Self::new(MemoryConfig::default()).unwrap()
    }
}

// ============================================================================
// RESULT TYPES
// ============================================================================

/// Result of cache spill operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpillResult {
    /// Memory freed in bytes
    pub memory_freed: usize,

    /// Names of caches cleared
    pub caches_cleared: Vec<String>,

    /// Number of entries spilled
    pub entries_spilled: usize,
}

/// Memory usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    /// Current RSS memory in bytes
    pub rss_bytes: usize,

    /// Total system memory in bytes
    pub total_bytes: usize,

    /// Number of cache entries in memory
    pub cache_entries: usize,

    /// Size of cache in memory (bytes)
    pub cache_bytes: usize,

    /// Number of spilled entries on disk
    pub spilled_entries: usize,

    /// Size of spilled cache on disk (bytes)
    pub spilled_bytes: usize,
}

impl MemoryStats {
    /// Get memory usage as a percentage
    pub fn memory_percent(&self) -> f64 {
        (self.rss_bytes as f64 / self.total_bytes as f64) * 100.0
    }
}

// ============================================================================
// ERROR TYPES
// ============================================================================

/// Memory management errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Process access failed: {0}")]
    ProcessAccess(String),

    #[error("Failed to get memory info: {0}")]
    MemoryInfo(String),

    #[error("Spill operation failed: {0}")]
    SpillFailed(String),

    #[error("Cache entry not found: {0}")]
    CacheNotFound(String),
}

// ============================================================================
// SPILL STRATEGY
// ============================================================================

/// Memory-aware spilling strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpillStrategy {
    /// Clear all caches
    ClearAll,

    /// Clear only non-active project caches
    NonActiveProjects,

    /// LRU-based eviction (default)
    LRU,
}

/// Memory monitoring for proactive spilling
pub struct MemoryMonitor {
    spiller: CacheSpiller,
    strategy: SpillStrategy,
}

impl MemoryMonitor {
    /// Create a new memory monitor
    pub fn new(spiller: CacheSpiller, strategy: SpillStrategy) -> Self {
        Self { spiller, strategy }
    }

    /// Check memory and spill if needed
    pub fn check_and_spill(&mut self) -> Result<Option<SpillResult>, Error> {
        if self.spiller.memory_manager.is_threshold_exceeded()? {
            match self.strategy {
                SpillStrategy::ClearAll => Ok(Some(self.spiller.spill_all()?)),
                SpillStrategy::NonActiveProjects => Ok(Some(self.spiller.check_and_spill()?)),
                SpillStrategy::LRU => Ok(Some(self.spiller.check_and_spill()?)),
            }
        } else {
            Ok(None)
        }
    }

    /// Get memory statistics
    pub fn stats(&self) -> Result<MemoryStats, Error> {
        self.spiller.memory_stats()
    }
}

// ============================================================================
// HELPERS FOR INTEGRATION
// ============================================================================

/// Helper to create a PDG cache entry
pub fn create_pdg_entry(
    project_id: String,
    node_count: usize,
    edge_count: usize,
    pdg_data: &[u8],
) -> CacheEntry {
    CacheEntry::PDG {
        project_id,
        node_count,
        edge_count,
        serialized_data: pdg_data.to_vec(),
    }
}

/// Helper to create a search index cache entry
pub fn create_search_entry(
    project_id: String,
    entry_count: usize,
    index_data: &[u8],
) -> CacheEntry {
    CacheEntry::SearchIndex {
        project_id,
        entry_count,
        serialized_data: index_data.to_vec(),
    }
}

/// Generate cache key for a project PDG
pub fn pdg_cache_key(project_id: &str) -> String {
    format!("pdg:{}", project_id)
}

/// Generate cache key for a project search index
pub fn search_cache_key(project_id: &str) -> String {
    format!("search:{}", project_id)
}

/// Generate cache key for an analysis result
pub fn analysis_cache_key(query: &str) -> String {
    format!("analysis:{}", sanitize_key(query))
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_manager_creation() {
        let manager = MemoryManager::new(MemoryConfig::default());
        assert!(manager.is_ok());
    }

    #[test]
    fn test_memory_info() {
        let manager = MemoryManager::new(MemoryConfig::default()).unwrap();
        let rss = manager.get_rss_bytes();
        assert!(rss.is_ok());
        assert!(rss.unwrap() > 0);
    }

    #[test]
    fn test_spill_config_default() {
        let config = MemoryConfig::default();
        assert_eq!(config.spill_threshold, 0.85);
        assert_eq!(config.check_interval_secs, 30);
        assert!(config.auto_spill);
        assert_eq!(config.max_cache_bytes, 500_000_000);
    }

    #[test]
    fn test_cache_entry_size() {
        let entry = CacheEntry::Binary {
            metadata: HashMap::new(),
            serialized_data: vec![0u8; 1024],
        };

        assert_eq!(entry.size_bytes(), 1024);
    }

    #[test]
    fn test_cache_store_insert_get() {
        let config = MemoryConfig {
            max_cache_bytes: 10_000,
            ..Default::default()
        };

        let mut store = CacheStore::new(&config);

        let entry = CacheEntry::Binary {
            metadata: HashMap::new(),
            serialized_data: vec![0u8; 100],
        };

        store.insert("test_key".to_string(), entry.clone()).unwrap();

        let retrieved = store.get("test_key");
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_cache_key_sanitization() {
        assert_eq!(sanitize_key("test/key"), "test_key");
        assert_eq!(sanitize_key("test:key"), "test_key");
        assert_eq!(sanitize_key("test key"), "test_key");
    }

    #[test]
    fn test_cache_key_generation() {
        assert_eq!(pdg_cache_key("myproject"), "pdg:myproject");
        assert_eq!(search_cache_key("myproject"), "search:myproject");
        assert!(analysis_cache_key("how does auth work").starts_with("analysis:"));
    }

    #[test]
    fn test_memory_stats() {
        let stats = MemoryStats {
            rss_bytes: 1_000_000,
            total_bytes: 8_000_000_000,
            cache_entries: 10,
            cache_bytes: 50_000,
            spilled_entries: 5,
            spilled_bytes: 25_000,
        };

        let percent = stats.memory_percent();
        assert!(percent > 0.0 && percent < 100.0);
    }
}
