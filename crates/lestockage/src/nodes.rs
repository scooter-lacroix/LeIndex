// Node persistence operations

use rusqlite::{params, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use crate::schema::Storage;

/// Node record for database storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    pub id: Option<i64>,
    pub project_id: String,
    pub file_path: String,
    pub symbol_name: String,
    pub node_type: NodeType,
    pub signature: Option<String>,
    pub complexity: Option<i32>,
    pub content_hash: String,
    pub embedding: Option<Vec<u8>>,
}

/// Node type enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeType {
    Function,
    Class,
    Method,
    Variable,
    Module,
}

impl NodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeType::Function => "function",
            NodeType::Class => "class",
            NodeType::Method => "method",
            NodeType::Variable => "variable",
            NodeType::Module => "module",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(NodeType::Function),
            "class" => Some(NodeType::Class),
            "method" => Some(NodeType::Method),
            "variable" => Some(NodeType::Variable),
            "module" => Some(NodeType::Module),
            _ => None,
        }
    }
}

/// Node store for CRUD operations
pub struct NodeStore<'a> {
    storage: &'a mut Storage,
}

impl<'a> NodeStore<'a> {
    /// Create a new node store
    pub fn new(storage: &'a mut Storage) -> Self {
        Self { storage }
    }

    /// Insert a node record
    pub fn insert(&mut self, record: &NodeRecord) -> SqliteResult<i64> {
        self.storage.conn().execute(
            "INSERT INTO intel_nodes (project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                record.project_id,
                record.file_path,
                record.symbol_name,
                record.node_type.as_str(),
                record.signature,
                record.complexity,
                record.content_hash,
                record.embedding.as_deref(),
                chrono::Utc::now().timestamp(),
                chrono::Utc::now().timestamp(),
            ],
        )?;

        Ok(self.storage.conn().last_insert_rowid())
    }

    /// Batch insert nodes
    pub fn batch_insert(&mut self, records: &[NodeRecord]) -> SqliteResult<Vec<i64>> {
        let tx = self.storage.conn_mut().transaction()?;

        let mut ids = Vec::new();
        for record in records {
            tx.execute(
                "INSERT INTO intel_nodes (project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    record.project_id,
                    record.file_path,
                    record.symbol_name,
                    record.node_type.as_str(),
                    record.signature,
                    record.complexity,
                    record.content_hash,
                    record.embedding.as_deref(),
                    chrono::Utc::now().timestamp(),
                    chrono::Utc::now().timestamp(),
                ],
            )?;
            ids.push(tx.last_insert_rowid());
        }

        tx.commit()?;
        Ok(ids)
    }

    /// Get node by ID
    pub fn get(&self, id: i64) -> SqliteResult<Option<NodeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding
             FROM intel_nodes WHERE id = ?1"
        )?;

        let result = stmt.query_row(params![id], |row| {
            Ok(NodeRecord {
                id: Some(row.get(0)?),
                project_id: row.get(1)?,
                file_path: row.get(2)?,
                symbol_name: row.get(3)?,
                node_type: NodeType::from_str(&row.get::<_, String>(4)?).unwrap_or(NodeType::Function),
                signature: row.get(5)?,
                complexity: row.get(6)?,
                content_hash: row.get(7)?,
                embedding: row.get(8)?,
            })
        });

        result.optional()
    }

    /// Find node by content hash
    pub fn find_by_hash(&self, hash: &str) -> SqliteResult<Option<NodeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding
             FROM intel_nodes WHERE content_hash = ?1"
        )?;

        let result = stmt.query_row(params![hash], |row| {
            Ok(NodeRecord {
                id: Some(row.get(0)?),
                project_id: row.get(1)?,
                file_path: row.get(2)?,
                symbol_name: row.get(3)?,
                node_type: NodeType::from_str(&row.get::<_, String>(4)?).unwrap_or(NodeType::Function),
                signature: row.get(5)?,
                complexity: row.get(6)?,
                content_hash: row.get(7)?,
                embedding: row.get(8)?,
            })
        });

        result.optional()
    }

    /// Get nodes by file path
    pub fn get_by_file(&self, file_path: &str) -> SqliteResult<Vec<NodeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding
             FROM intel_nodes WHERE file_path = ?1"
        )?;

        let nodes = stmt.query_map(params![file_path], |row| {
            Ok(NodeRecord {
                id: Some(row.get(0)?),
                project_id: row.get(1)?,
                file_path: row.get(2)?,
                symbol_name: row.get(3)?,
                node_type: NodeType::from_str(&row.get::<_, String>(4)?).unwrap_or(NodeType::Function),
                signature: row.get(5)?,
                complexity: row.get(6)?,
                content_hash: row.get(7)?,
                embedding: row.get(8)?,
            })
        })?.collect::<SqliteResult<Vec<_>>>()?;

        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Storage;
    use tempfile::NamedTempFile;

    #[test]
    fn test_node_insert_and_get() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let mut store = NodeStore::new(&mut storage);

        let record = NodeRecord {
            id: None,
            project_id: "test_project".to_string(),
            file_path: "test.py".to_string(),
            symbol_name: "test_func".to_string(),
            node_type: NodeType::Function,
            signature: Some("def test_func()".to_string()),
            complexity: Some(5),
            content_hash: "abc123".to_string(),
            embedding: None,
        };

        let id = store.insert(&record).unwrap();
        assert!(id > 0);

        let retrieved = store.get(id).unwrap().unwrap();
        assert_eq!(retrieved.symbol_name, "test_func");
    }

    #[test]
    fn test_find_by_hash() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let mut store = NodeStore::new(&mut storage);

        let record = NodeRecord {
            id: None,
            project_id: "test_project".to_string(),
            file_path: "test.py".to_string(),
            symbol_name: "test_func".to_string(),
            node_type: NodeType::Function,
            signature: None,
            complexity: None,
            content_hash: "hash123".to_string(),
            embedding: None,
        };

        store.insert(&record).unwrap();
        let found = store.find_by_hash("hash123").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().symbol_name, "test_func");
    }
}
