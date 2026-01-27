// DuckDB analytics integration

use rusqlite::{params, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use crate::schema::Storage;

/// Analytics for graph metrics
pub struct Analytics {
    storage: Storage,
}

impl Analytics {
    /// Create a new analytics instance
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Get node count by type
    pub fn count_nodes_by_type(&self) -> SqliteResult<Vec<NodeTypeCount>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT node_type, COUNT(*) as count FROM intel_nodes GROUP BY node_type"
        )?;

        let counts = stmt.query_map([], |row| {
            Ok(NodeTypeCount {
                node_type: row.get(0)?,
                count: row.get(1)?,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(counts)
    }

    /// Get complexity distribution
    pub fn complexity_distribution(&self) -> SqliteResult<Vec<ComplexityBucket>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT
                CASE
                    WHEN complexity < 5 THEN 'simple'
                    WHEN complexity < 10 THEN 'moderate'
                    WHEN complexity < 20 THEN 'complex'
                    ELSE 'very_complex'
                END as bucket,
                COUNT(*) as count
                FROM intel_nodes
                GROUP BY bucket
                ORDER BY bucket"
        )?;

        let buckets = stmt.query_map([], |row| {
            Ok(ComplexityBucket {
                bucket: row.get(0)?,
                count: row.get(1)?,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(buckets)
    }

    /// Get edge count by type
    pub fn count_edges_by_type(&self) -> SqliteResult<Vec<EdgeTypeCount>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT edge_type, COUNT(*) as count FROM intel_edges GROUP BY edge_type"
        )?;

        let counts = stmt.query_map([], |row| {
            Ok(EdgeTypeCount {
                edge_type: row.get(0)?,
                count: row.get(1)?,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(counts)
    }

    /// Get hotspots (high complexity + high centrality)
    pub fn get_hotspots(&self, threshold: i32) -> SqliteResult<Vec<Hotspot>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT
                n.id,
                n.symbol_name,
                n.file_path,
                n.complexity,
                COUNT(e.callee_id) as fan_out
                FROM intel_nodes n
                LEFT JOIN intel_edges e ON n.id = e.caller_id
                WHERE n.complexity >= ?1
                GROUP BY n.id
                HAVING fan_out > ?2
                ORDER BY n.complexity DESC, fan_out DESC"
        )?;

        let hotspots = stmt.query_map(params![threshold, threshold / 2], |row| {
            Ok(Hotspot {
                node_id: row.get(0)?,
                symbol_name: row.get(1)?,
                file_path: row.get(2)?,
                complexity: row.get(3)?,
                fan_out: row.get(4)?,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(hotspots)
    }
}

/// Node type count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTypeCount {
    /// Type of the node (as string)
    pub node_type: String,
    /// Number of nodes of this type
    pub count: i64,
}

/// Complexity bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityBucket {
    /// Complexity category (e.g., 'simple', 'moderate', etc.)
    pub bucket: String,
    /// Number of nodes in this complexity bucket
    pub count: i64,
}

/// Edge type count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTypeCount {
    /// Type of the edge (as string)
    pub edge_type: String,
    /// Number of edges of this type
    pub count: i64,
}

/// Hotspot node
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotspot {
    /// ID of the hotspot node
    pub node_id: i64,
    /// Name of the symbol
    pub symbol_name: String,
    /// Path to the file containing the symbol
    pub file_path: String,
    /// Complexity score of the node
    pub complexity: i32,
    /// Number of outgoing edges (fan-out)
    pub fan_out: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Storage;
    use tempfile::NamedTempFile;

    #[test]
    fn test_analytics_creation() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();
        let analytics = Analytics::new(storage);

        // Empty database should return empty results
        let counts = analytics.count_nodes_by_type().unwrap();
        assert_eq!(counts.len(), 0);
    }
}
