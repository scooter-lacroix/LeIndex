// Node persistence operations

use crate::schema::Storage;
use rusqlite::{params, OptionalExtension, Result as SqliteResult};
use serde::{Deserialize, Serialize};

/// Node record for database storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRecord {
    /// Unique database ID
    pub id: Option<i64>,
    /// Origin project ID
    pub project_id: String,
    /// Path to the file containing the node
    pub file_path: String,
    /// Unique node ID (file_path:qualified_name)
    pub node_id: String,
    /// Name of the symbol (short name)
    pub symbol_name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Programming language
    pub language: String,
    /// Type of the node (function, class, etc.)
    pub node_type: NodeType,
    /// Code signature or declaration
    pub signature: Option<String>,
    /// Complexity score
    pub complexity: Option<i32>,
    /// Content hash for change detection
    pub content_hash: String,
    /// Vector embedding for semantic search
    pub embedding: Option<Vec<u8>>,
    /// Byte range start
    pub byte_range_start: Option<i64>,
    /// Byte range end
    pub byte_range_end: Option<i64>,
}

/// Node type enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeType {
    /// A function definition
    Function,
    /// A class definition
    Class,
    /// A method definition
    Method,
    /// A variable definition
    Variable,
    /// A module or file
    Module,
}

impl NodeType {
    /// Return the string representation of the node type.
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeType::Function => "function",
            NodeType::Class => "class",
            NodeType::Method => "method",
            NodeType::Variable => "variable",
            NodeType::Module => "module",
        }
    }

    /// Create a node type from its string representation.
    pub fn from_str_name(s: &str) -> Option<Self> {
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
            "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, signature, complexity, content_hash, embedding, byte_range_start, byte_range_end, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                record.project_id,
                record.file_path,
                record.node_id,
                record.symbol_name,
                record.qualified_name,
                record.language,
                record.node_type.as_str(),
                record.signature,
                record.complexity,
                record.content_hash,
                record.embedding.as_deref(),
                record.byte_range_start,
                record.byte_range_end,
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
                "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, signature, complexity, content_hash, embedding, byte_range_start, byte_range_end, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    record.project_id,
                    record.file_path,
                    record.node_id,
                    record.symbol_name,
                    record.qualified_name,
                    record.language,
                    record.node_type.as_str(),
                    record.signature,
                    record.complexity,
                    record.content_hash,
                    record.embedding.as_deref(),
                    record.byte_range_start,
                    record.byte_range_end,
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
            "SELECT id, project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, signature, complexity, content_hash, embedding, byte_range_start, byte_range_end
             FROM intel_nodes WHERE id = ?1"
        )?;

        let result = stmt.query_row(params![id], |row| {
            Ok(NodeRecord {
                id: Some(row.get(0)?),
                project_id: row.get(1)?,
                file_path: row.get(2)?,
                node_id: row.get(3)?,
                symbol_name: row.get(4)?,
                qualified_name: row.get(5)?,
                language: row.get(6)?,
                node_type: NodeType::from_str_name(&row.get::<_, String>(7)?)
                    .unwrap_or(NodeType::Function),
                signature: row.get(8)?,
                complexity: row.get(9)?,
                content_hash: row.get(10)?,
                embedding: row.get(11)?,
                byte_range_start: row.get(12)?,
                byte_range_end: row.get(13)?,
            })
        });

        result.optional()
    }

    /// Find node by content hash
    pub fn find_by_hash(&self, hash: &str) -> SqliteResult<Option<NodeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, signature, complexity, content_hash, embedding, byte_range_start, byte_range_end
             FROM intel_nodes WHERE content_hash = ?1"
        )?;

        let result = stmt.query_row(params![hash], |row| {
            Ok(NodeRecord {
                id: Some(row.get(0)?),
                project_id: row.get(1)?,
                file_path: row.get(2)?,
                node_id: row.get(3)?,
                symbol_name: row.get(4)?,
                qualified_name: row.get(5)?,
                language: row.get(6)?,
                node_type: NodeType::from_str_name(&row.get::<_, String>(7)?)
                    .unwrap_or(NodeType::Function),
                signature: row.get(8)?,
                complexity: row.get(9)?,
                content_hash: row.get(10)?,
                embedding: row.get(11)?,
                byte_range_start: row.get(12)?,
                byte_range_end: row.get(13)?,
            })
        });

        result.optional()
    }

    /// Get nodes by file path
    pub fn get_by_file(&self, file_path: &str) -> SqliteResult<Vec<NodeRecord>> {
        let mut stmt = self.storage.conn().prepare(
            "SELECT id, project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, signature, complexity, content_hash, embedding, byte_range_start, byte_range_end
             FROM intel_nodes WHERE file_path = ?1"
        )?;

        let nodes = stmt
            .query_map(params![file_path], |row| {
                Ok(NodeRecord {
                    id: Some(row.get(0)?),
                    project_id: row.get(1)?,
                    file_path: row.get(2)?,
                    node_id: row.get(3)?,
                    symbol_name: row.get(4)?,
                    qualified_name: row.get(5)?,
                    language: row.get(6)?,
                    node_type: NodeType::from_str_name(&row.get::<_, String>(7)?)
                        .unwrap_or(NodeType::Function),
                    signature: row.get(8)?,
                    complexity: row.get(9)?,
                    content_hash: row.get(10)?,
                    embedding: row.get(11)?,
                    byte_range_start: row.get(12)?,
                    byte_range_end: row.get(13)?,
                })
            })?
            .collect::<SqliteResult<Vec<_>>>()?;

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
            node_id: "test_project:test_func".to_string(),
            symbol_name: "test_func".to_string(),
            qualified_name: "test_func".to_string(),
            language: "python".to_string(),
            node_type: NodeType::Function,
            signature: Some("def test_func()".to_string()),
            complexity: Some(5),
            content_hash: "abc123".to_string(),
            embedding: None,
            byte_range_start: Some(0),
            byte_range_end: Some(100),
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
            node_id: "test_project:test_func".to_string(),
            symbol_name: "test_func".to_string(),
            qualified_name: "test_func".to_string(),
            language: "python".to_string(),
            node_type: NodeType::Function,
            signature: None,
            complexity: None,
            content_hash: "hash123".to_string(),
            embedding: None,
            byte_range_start: Some(0),
            byte_range_end: Some(100),
        };

        store.insert(&record).unwrap();
        let found = store.find_by_hash("hash123").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().symbol_name, "test_func");
    }
}
