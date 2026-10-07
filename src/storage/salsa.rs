// Salsa incremental computation

use crate::storage::schema::Storage;
use rusqlite::{OptionalExtension, Result as SqliteResult, params};
use serde::{Deserialize, Serialize};

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

    /// Parse from hex string
    pub fn from_str_name(s: &str) -> Option<Self> {
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
        let mut stmt = self
            .storage
            .conn()
            .prepare("SELECT COUNT(*) FROM analysis_cache WHERE node_hash = ?1")?;

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

        let optional = result.optional()?;
        if optional.is_some() {
            let _ = self.bump_cache_hits();
        } else {
            let _ = self.bump_cache_misses();
        }
        Ok(optional)
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
        let _ = self.bump_cache_writes();
        Ok(())
    }

    fn bump_cache_hits(&self) -> SqliteResult<usize> {
        self.storage.conn().execute(
            "UPDATE cache_telemetry
             SET cache_hits = cache_hits + 1,
                 updated_at = strftime('%s', 'now')
             WHERE id = 1",
            [],
        )
    }

    fn bump_cache_misses(&self) -> SqliteResult<usize> {
        self.storage.conn().execute(
            "UPDATE cache_telemetry
             SET cache_misses = cache_misses + 1,
                 updated_at = strftime('%s', 'now')
             WHERE id = 1",
            [],
        )
    }

    fn bump_cache_writes(&self) -> SqliteResult<usize> {
        self.storage.conn().execute(
            "UPDATE cache_telemetry
             SET cache_writes = cache_writes + 1,
                 updated_at = strftime('%s', 'now')
             WHERE id = 1",
            [],
        )
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
        let result = self
            .storage
            .conn()
            .execute("DELETE FROM analysis_cache", [])?;
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
///
/// This system tracks dependencies between nodes and allows for
/// targeted invalidation of cached analysis results when source code changes.
pub struct QueryInvalidation {
    storage: Storage,
}

impl QueryInvalidation {
    /// Create a new invalidation system
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Invalidate a node and its dependents
    ///
    /// # Arguments
    /// * `node_hash` - The hash of the node to invalidate
    pub fn invalidate_node(&mut self, node_hash: &NodeHash) -> SqliteResult<()> {
        // Remove from cache
        self.storage.conn().execute(
            "DELETE FROM analysis_cache WHERE node_hash = ?1",
            params![node_hash.as_str()],
        )?;
        Ok(())
    }

    /// Get content hashes for the nodes in a changed file.
    ///
    /// Replaces the former `SELECT content_hash FROM intel_nodes WHERE
    /// file_path = ?` query. The generation PDG is authoritative for graph
    /// identity; reconstruct the legacy content hash from the same content
    /// fields used by the writer rather than consulting graph rows in SQL.
    pub fn get_affected_nodes(
        &self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        file_path: &str,
    ) -> Vec<String> {
        let mut hashes = Vec::new();
        for node_index in pdg.node_indices() {
            let Some(node) = pdg.get_node(node_index) else {
                continue;
            };
            if node.file_path.as_ref() != file_path {
                continue;
            }

            let qualified_name = node.id.rsplit(':').next().unwrap_or(&node.id);
            let node_type =
                crate::storage::generation::graph_codec::graph_node_type_str(&node.node_type);
            let mut hasher = blake3::Hasher::new();
            for field in [
                node.file_path.as_bytes(),
                node.name.as_bytes(),
                qualified_name.as_bytes(),
                node.language.as_bytes(),
                node_type.as_bytes(),
            ] {
                hasher.update(field);
                hasher.update(&[0x1f]);
            }
            hasher.update(&node.complexity.to_le_bytes());
            hasher.update(&[0x1f]);
            hasher.update(&node.byte_range.0.to_le_bytes());
            hasher.update(&[0x1f]);
            hasher.update(&node.byte_range.1.to_le_bytes());
            hashes.push(hasher.finalize().to_hex().to_string());
        }
        hashes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Storage;
    use tempfile::NamedTempFile;

    #[test]
    fn test_affected_node_hashes_come_from_resident_graph_and_match_writer_fields() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
        use std::sync::Arc;

        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();
        let invalidation = QueryInvalidation::new(storage);
        let mut pdg = ProgramDependenceGraph::new();
        pdg.add_node(Node {
            id: "src/lib.rs:module::function".to_string(),
            node_type: NodeType::Function,
            name: "function".to_string(),
            file_path: Arc::from("src/lib.rs"),
            byte_range: (4, 32),
            complexity: 7,
            language: "rust".to_string(),
        });

        let hashes = invalidation.get_affected_nodes(&pdg, "src/lib.rs");
        let mut expected = blake3::Hasher::new();
        for field in [
            b"src/lib.rs".as_slice(),
            b"function".as_slice(),
            b"function".as_slice(),
            b"rust".as_slice(),
            b"function".as_slice(),
        ] {
            expected.update(field);
            expected.update(&[0x1f]);
        }
        expected.update(&7u32.to_le_bytes());
        expected.update(&[0x1f]);
        expected.update(&4usize.to_le_bytes());
        expected.update(&[0x1f]);
        expected.update(&32usize.to_le_bytes());

        assert_eq!(hashes, vec![expected.finalize().to_hex().to_string()]);
        assert!(
            invalidation
                .get_affected_nodes(&pdg, "src/missing.rs")
                .is_empty()
        );
    }

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

        let telemetry: (i64, i64, i64) = cache
            .storage
            .conn()
            .query_row(
                "SELECT cache_hits, cache_misses, cache_writes FROM cache_telemetry WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(telemetry.0 >= 1, "expected cache hit telemetry");
        assert!(telemetry.2 >= 1, "expected cache write telemetry");
    }
}
