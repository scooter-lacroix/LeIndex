// Salsa incremental computation

use blake3::Hash;
use rusqlite::{params, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use crate::schema::Storage;

/// Node hash for incremental computation
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeHash(String);

impl NodeHash {
    /// Create a new hash from bytes
    pub fn new(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        Self(hash.to_hex().to_string())
    }

    /// Get the hash string
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse from string
    pub fn from_str(s: &str) -> Option<Self> {
        if s.len() == 64 {
            Some(Self(s.to_string()))
        } else {
            None
        }
    }
}

/// Incremental computation cache
pub struct IncrementalCache {
    storage: Storage,
}

impl IncrementalCache {
    /// Create a new cache
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Check if a node's computation is cached
    pub fn is_cached(&self, hash: &NodeHash) -> SqliteResult<bool> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT COUNT(*) FROM analysis_cache WHERE node_hash = ?1"
        )?;

        let count: i64 = stmt.query_row(params![hash.as_str()], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Get cached computation result
    pub fn get(&self, hash: &NodeHash) -> SqliteResult<Option<CachedComputation>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT cfg_data, complexity_metrics, timestamp FROM analysis_cache WHERE node_hash = ?1"
        )?;

        let result = stmt.query_row(params![hash.as_str()], |row| {
            Ok(CachedComputation {
                cfg_data: row.get(0)?,
                complexity_metrics: row.get(1)?,
                timestamp: row.get(2)?,
            })
        });

        result.optional()
    }

    /// Store computation result in cache
    pub fn put(&mut self, hash: &NodeHash, computation: &CachedComputation) -> SqliteResult<()> {
        self.storage.conn().execute(
            "INSERT INTO analysis_cache (node_hash, cfg_data, complexity_metrics, timestamp)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO UPDATE SET
                     cfg_data = excluded.cfg_data,
                     complexity_metrics = excluded.complexity_metrics,
                     timestamp = excluded.timestamp",
            params![
                hash.as_str(),
                computation.cfg_data,
                computation.complexity_metrics,
                computation.timestamp,
            ],
        )?;
        Ok(())
    }

    /// Invalidate cached entries older than timestamp
    pub fn invalidate_before(&mut self, timestamp: i64) -> SqliteResult<usize> {
        let result = self.storage.conn().execute(
            "DELETE FROM analysis_cache WHERE timestamp < ?1",
            params![timestamp],
        )?;
        Ok(result)
    }

    /// Clear all cached entries
    pub fn clear(&mut self) -> SqliteResult<usize> {
        let result = self.storage.conn().execute("DELETE FROM analysis_cache", [])?;
        Ok(result)
    }
}

/// Cached computation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedComputation {
    /// CFG data (serialized)
    pub cfg_data: Option<Vec<u8>>,

    /// Complexity metrics (serialized)
    pub complexity_metrics: Option<Vec<u8>>,

    /// Timestamp when cached
    pub timestamp: i64,
}

/// Query-based invalidation system
pub struct QueryInvalidation {
    storage: Storage,
}

impl QueryInvalidation {
    /// Create a new invalidation system
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Invalidate a node and its dependents
    pub fn invalidate_node(&mut self, node_hash: &NodeHash) -> SqliteResult<()> {
        // Remove from cache
        self.storage.conn().execute(
            "DELETE FROM analysis_cache WHERE node_hash = ?1",
            params![node_hash.as_str()],
        )?;
        Ok(())
    }

    /// Get affected nodes for a change
    pub fn get_affected_nodes(&self, file_path: &str) -> SqliteResult<Vec<String>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT content_hash FROM intel_nodes WHERE file_path = ?1"
        )?;

        let hashes = stmt.query_map(params![file_path], |row| {
            Ok(row.get::<_, String>(0)?)
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Storage;
    use tempfile::NamedTempFile;

    #[test]
    fn test_node_hash_creation() {
        let data = b"hello world";
        let hash = NodeHash::new(data);
        assert_eq!(hash.as_str().len(), 64);
    }

    #[test]
    fn test_incremental_cache() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();
        let mut cache = IncrementalCache::new(storage);

        let hash = NodeHash::new(b"test data");
        let computation = CachedComputation {
            cfg_data: Some(vec![1, 2, 3]),
            complexity_metrics: Some(vec![4, 5, 6]),
            timestamp: chrono::Utc::now().timestamp(),
        };

        cache.put(&hash, &computation).unwrap();
        assert!(cache.is_cached(&hash).unwrap());

        let retrieved = cache.get(&hash).unwrap().unwrap();
        assert_eq!(retrieved.cfg_data, Some(vec![1, 2, 3]));
    }
}
