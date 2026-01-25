// Edge persistence operations

use rusqlite::{params, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use crate::schema::Storage;

/// Edge record for database storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub caller_id: i64,
    pub callee_id: i64,
    pub edge_type: EdgeType,
    pub metadata: Option<EdgeMetadata>,
}

/// Edge type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EdgeType {
    Call,
    DataDependency,
    Inheritance,
    Import,
}

impl EdgeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeType::Call => "call",
            EdgeType::DataDependency => "data_dependency",
            EdgeType::Inheritance => "inheritance",
            EdgeType::Import => "import",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "call" => Some(EdgeType::Call),
            "data_dependency" => Some(EdgeType::DataDependency),
            "inheritance" => Some(EdgeType::Inheritance),
            "import" => Some(EdgeType::Import),
            _ => None,
        }
    }
}

/// Edge metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMetadata {
    pub call_count: Option<usize>,
    pub variable_name: Option<String>,
}

/// Edge store for CRUD operations
pub struct EdgeStore<'a> {
    storage: &'a mut Storage,
}

impl<'a> EdgeStore<'a> {
    /// Create a new edge store
    pub fn new(storage: &'a mut Storage) -> Self {
        Self { storage }
    }

    /// Insert an edge record
    pub fn insert(&mut self, record: &EdgeRecord) -> SqliteResult<()> {
        let metadata_json = serde_json::to_string(&record.metadata).ok();
        self.storage.conn().execute(
            "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO UPDATE SET metadata = excluded.metadata",
            params![
                record.caller_id,
                record.callee_id,
                record.edge_type.as_str(),
                metadata_json,
            ],
        )?;
        Ok(())
    }

    /// Batch insert edges
    pub fn batch_insert(&mut self, records: &[EdgeRecord]) -> SqliteResult<()> {
        let tx = self.storage.conn_mut().transaction()?;

        for record in records {
            let metadata_json = serde_json::to_string(&record.metadata).ok();
            tx.execute(
                "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT DO UPDATE SET metadata = excluded.metadata",
                params![
                    record.caller_id,
                    record.callee_id,
                    record.edge_type.as_str(),
                    metadata_json,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Get edges by caller ID
    pub fn get_by_caller(&self, caller_id: i64) -> SqliteResult<Vec<EdgeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT caller_id, callee_id, edge_type, metadata
             FROM intel_edges WHERE caller_id = ?1"
        )?;

        let edges = stmt.query_map(params![caller_id], |row| {
            let edge_type_str: String = row.get(2)?;
            let metadata_json: Option<String> = row.get(3)?;
            let metadata = metadata_json
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(EdgeRecord {
                caller_id: row.get(0)?,
                callee_id: row.get(1)?,
                edge_type: EdgeType::from_str(&edge_type_str).unwrap_or(EdgeType::Call),
                metadata,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(edges)
    }

    /// Get edges by callee ID (incoming edges)
    pub fn get_by_callee(&self, callee_id: i64) -> SqliteResult<Vec<EdgeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT caller_id, callee_id, edge_type, metadata
             FROM intel_edges WHERE callee_id = ?1"
        )?;

        let edges = stmt.query_map(params![callee_id], |row| {
            let edge_type_str: String = row.get(2)?;
            let metadata_json: Option<String> = row.get(3)?;
            let metadata = metadata_json
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(EdgeRecord {
                caller_id: row.get(0)?,
                callee_id: row.get(1)?,
                edge_type: EdgeType::from_str(&edge_type_str).unwrap_or(EdgeType::Call),
                metadata,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(edges)
    }

    /// Get edges by type
    pub fn get_by_type(&self, edge_type: EdgeType) -> SqliteResult<Vec<EdgeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT caller_id, callee_id, edge_type, metadata
             FROM intel_edges WHERE edge_type = ?1"
        )?;

        let edges = stmt.query_map(params![edge_type.as_str()], |row| {
            let edge_type_str: String = row.get(2)?;
            let metadata_json: Option<String> = row.get(3)?;
            let metadata = metadata_json
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(EdgeRecord {
                caller_id: row.get(0)?,
                callee_id: row.get(1)?,
                edge_type: EdgeType::from_str(&edge_type_str).unwrap_or(EdgeType::Call),
                metadata,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(edges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Storage;
    use tempfile::NamedTempFile;

    #[test]
    fn test_edge_insert_and_get() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let mut store = EdgeStore::new(&mut storage);

        let record = EdgeRecord {
            caller_id: 1,
            callee_id: 2,
            edge_type: EdgeType::Call,
            metadata: Some(EdgeMetadata {
                call_count: Some(5),
                variable_name: None,
            }),
        };

        store.insert(&record).unwrap();
        let edges = store.get_by_caller(1).unwrap();

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].callee_id, 2);
    }
}
