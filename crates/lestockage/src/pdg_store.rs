// PDG Persistence Bridge
//
// *Le Pont* (The Bridge) - Converts between legraphe PDG and lestockage records

use crate::edges::{EdgeType as StorageEdgeType, EdgeMetadata as StorageEdgeMetadata};
use crate::nodes::{NodeRecord, NodeType as StorageNodeType};
use crate::schema::Storage;
use legraphe::pdg::{
    ProgramDependenceGraph, Node as PDGNode, Edge as PDGEdge,
    NodeType as PDGNodeType, EdgeType as PDGEdgeType, EdgeMetadata as PDGEdgeMetadata,
    NodeId,
};
use rusqlite::{params, Result as SqliteResult};
use std::collections::HashMap;

/// Type alias for node database rows to reduce type complexity
type NodeDbRow = (i64, String, String, String, Option<i32>, String, Option<Vec<u8>>);

/// Errors that can occur during PDG persistence
#[derive(Debug, thiserror::Error)]
pub enum PdgStoreError {
    /// Error originating from the underlying SQLite database
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The specified node ID was not found in the database
    #[error("Node not found: {0}")]
    NodeNotFound(i64),

    /// An edge refers to a node that does not exist in the database
    #[error("Edge refers to non-existent node: caller={caller}, callee={callee}")]
    EdgeNodeMissing {
        /// ID of the caller node
        caller: i64,
        /// ID of the callee node
        callee: i64,
    },

    /// Failed to serialize PDG data for storage
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Failed to deserialize stored data back into a PDG
    #[error("Deserialization error: {0}")]
    Deserialization(String),
}

/// Result type for PDG store operations
pub type Result<T> = std::result::Result<T, PdgStoreError>;

/// Convert legraphe NodeType to lestockage NodeType
fn convert_node_type(node_type: &PDGNodeType) -> StorageNodeType {
    match node_type {
        PDGNodeType::Function => StorageNodeType::Function,
        PDGNodeType::Class => StorageNodeType::Class,
        PDGNodeType::Method => StorageNodeType::Method,
        PDGNodeType::Variable => StorageNodeType::Variable,
        PDGNodeType::Module => StorageNodeType::Module,
    }
}

/// Convert lestockage NodeType to legraphe NodeType
fn convert_storage_node_type(node_type: &StorageNodeType) -> PDGNodeType {
    match node_type {
        StorageNodeType::Function => PDGNodeType::Function,
        StorageNodeType::Class => PDGNodeType::Class,
        StorageNodeType::Method => PDGNodeType::Method,
        StorageNodeType::Variable => PDGNodeType::Variable,
        StorageNodeType::Module => PDGNodeType::Module,
    }
}

/// Convert legraphe EdgeType to lestockage EdgeType
fn convert_edge_type(edge_type: &PDGEdgeType) -> StorageEdgeType {
    match edge_type {
        PDGEdgeType::Call => StorageEdgeType::Call,
        PDGEdgeType::DataDependency => StorageEdgeType::DataDependency,
        PDGEdgeType::Inheritance => StorageEdgeType::Inheritance,
        PDGEdgeType::Import => StorageEdgeType::Import,
    }
}

/// Convert lestockage EdgeType to legraphe EdgeType
fn convert_storage_edge_type(edge_type: &StorageEdgeType) -> PDGEdgeType {
    match edge_type {
        StorageEdgeType::Call => PDGEdgeType::Call,
        StorageEdgeType::DataDependency => PDGEdgeType::DataDependency,
        StorageEdgeType::Inheritance => PDGEdgeType::Inheritance,
        StorageEdgeType::Import => PDGEdgeType::Import,
    }
}

/// Convert legraphe EdgeMetadata to lestockage EdgeMetadata
fn convert_edge_metadata(metadata: &PDGEdgeMetadata) -> StorageEdgeMetadata {
    StorageEdgeMetadata {
        call_count: metadata.call_count,
        variable_name: metadata.variable_name.clone(),
    }
}

/// Convert lestockage EdgeMetadata to legraphe EdgeMetadata
fn convert_storage_edge_metadata(metadata: &StorageEdgeMetadata) -> PDGEdgeMetadata {
    PDGEdgeMetadata {
        call_count: metadata.call_count,
        variable_name: metadata.variable_name.clone(),
    }
}

/// Save a ProgramDependenceGraph to storage
///
/// This function extracts all nodes and edges from the PDG and persists them
/// to the SQLite database. All previous nodes and edges for the project are
/// replaced with the new PDG data.
///
/// # Arguments
///
/// * `storage` - Mutable reference to the storage backend
/// * `project_id` - Project identifier for the PDG
/// * `pdg` - Reference to the ProgramDependenceGraph to save
///
/// # Returns
///
/// `Ok(())` if successful, `Err(PdgStoreError)` if an error occurs
///
/// # Example
///
/// ```ignore
/// let pdg = extract_pdg_from_signatures(signatures, source, "test.rs");
/// save_pdg(&mut storage, "my_project", &pdg)?;
/// ```
pub fn save_pdg(
    storage: &mut Storage,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<()> {
    let tx = storage.conn_mut().transaction()?;

    // Delete existing edges for this project first (to avoid foreign key constraints)
    tx.execute(
        "DELETE FROM intel_edges WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
        params![project_id],
    )?;

    // Then delete existing nodes for this project
    tx.execute(
        "DELETE FROM intel_nodes WHERE project_id = ?1",
        params![project_id],
    )?;

    // Insert all nodes
    let mut node_id_map: HashMap<NodeId, i64> = HashMap::new();

    for node_idx in pdg.node_indices() {
        let pdg_node = pdg.get_node(node_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Missing node data".to_string()))?;

        // Convert embedding to BLOB
        let embedding_blob = pdg_node.embedding.as_ref().map(|emb| {
            let mut blob = Vec::with_capacity(emb.len() * 4);
            for &val in emb {
                blob.extend_from_slice(&val.to_le_bytes());
            }
            blob
        });

        let record = NodeRecord {
            id: None,
            project_id: project_id.to_string(),
            file_path: pdg_node.file_path.clone(),
            symbol_name: pdg_node.name.clone(),
            node_type: convert_node_type(&pdg_node.node_type),
            signature: None, // Could be populated from node content
            complexity: Some(pdg_node.complexity as i32),
            content_hash: blake3::hash(pdg_node.id.as_bytes()).to_hex().to_string(),
            embedding: embedding_blob,
        };

        let db_id: i64 = tx.query_row(
            "INSERT INTO intel_nodes (project_id, file_path, symbol_name, node_type, signature, complexity, content_hash, embedding, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             RETURNING id",
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
            |row| row.get(0),
        )?;

        node_id_map.insert(node_idx, db_id);
    }

    // Insert all edges
    for edge_idx in pdg.edge_indices() {
        let (source, target) = pdg.edge_endpoints(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Edge has no endpoints".to_string()))?;

        let pdg_edge = pdg.get_edge(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Missing edge data".to_string()))?;

        let caller_id = *node_id_map.get(&source)
            .ok_or_else(|| PdgStoreError::EdgeNodeMissing {
                caller: source.index() as i64,
                callee: target.index() as i64,
            })?;

        let callee_id = *node_id_map.get(&target)
            .ok_or_else(|| PdgStoreError::EdgeNodeMissing {
                caller: source.index() as i64,
                callee: target.index() as i64,
            })?;

        let metadata = convert_edge_metadata(&pdg_edge.metadata);
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|e| PdgStoreError::Serialization(e.to_string()))?;

        tx.execute(
            "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT DO UPDATE SET metadata = excluded.metadata",
            params![
                caller_id,
                callee_id,
                convert_edge_type(&pdg_edge.edge_type).as_str(),
                metadata_json,
            ],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Load a ProgramDependenceGraph from storage
///
/// This function reconstructs a PDG from the SQLite database by loading all
/// nodes and edges for a given project. It rebuilds the StableGraph structure
/// along with the symbol_index and file_index.
///
/// # Arguments
///
/// * `storage` - Reference to the storage backend
/// * `project_id` - Project identifier to load
///
/// # Returns
///
/// `Ok(ProgramDependenceGraph)` if successful, `Err(PdgStoreError)` if an error occurs
///
/// # Example
///
/// ```ignore
/// let pdg = load_pdg(&storage, "my_project")?;
/// println!("Loaded {} nodes and {} edges", pdg.node_count(), pdg.edge_count());
/// ```
pub fn load_pdg(storage: &Storage, project_id: &str) -> Result<ProgramDependenceGraph> {
    let mut pdg = ProgramDependenceGraph::new();
    let mut db_id_to_node_id: HashMap<i64, NodeId> = HashMap::new();

    // Load all nodes for the project
    let mut nodes_stmt = storage.conn().prepare(
        "SELECT id, file_path, symbol_name, node_type, complexity, content_hash, embedding
         FROM intel_nodes WHERE project_id = ?1"
    )?;

    let node_rows: Vec<NodeDbRow> = nodes_stmt.query_map(params![project_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,           // id
            row.get::<_, String>(1)?,         // file_path
            row.get::<_, String>(2)?,         // symbol_name
            row.get::<_, String>(3)?,         // node_type
            row.get::<_, Option<i32>>(4)?,    // complexity
            row.get::<_, String>(5)?,         // content_hash
            row.get::<_, Option<Vec<u8>>>(6)?, // embedding
        ))
    })?.collect::<SqliteResult<Vec<_>>>()?;

    for (db_id, file_path, symbol_name, node_type_str, complexity, _content_hash, embedding_blob) in node_rows {
        let node_type = StorageNodeType::from_str_name(&node_type_str)
            .ok_or_else(|| PdgStoreError::Deserialization(format!("Invalid node type: {}", node_type_str)))?;

        // Convert embedding BLOB back to Vec<f32>
        let embedding = embedding_blob.and_then(|blob| {
            if blob.len() % 4 != 0 {
                return None;
            }
            let mut emb = Vec::with_capacity(blob.len() / 4);
            for chunk in blob.chunks_exact(4) {
                let bytes: [u8; 4] = [chunk[0], chunk[1], chunk[2], chunk[3]];
                emb.push(f32::from_le_bytes(bytes));
            }
            Some(emb)
        });

        let pdg_node = PDGNode {
            id: symbol_name.clone(),
            node_type: convert_storage_node_type(&node_type),
            name: symbol_name.clone(),
            file_path,
            byte_range: (0, 0), // Not stored in DB
            complexity: complexity.unwrap_or(0) as u32,
            embedding,
        };

        let node_id = pdg.add_node(pdg_node);
        db_id_to_node_id.insert(db_id, node_id);
    }

    // Load all edges for the project using a JOIN query
    let mut edges_stmt = storage.conn().prepare(
        "SELECT e.caller_id, e.callee_id, e.edge_type, e.metadata
         FROM intel_edges e
         INNER JOIN intel_nodes n1 ON e.caller_id = n1.id
         INNER JOIN intel_nodes n2 ON e.callee_id = n2.id
         WHERE n1.project_id = ?1 AND n2.project_id = ?1"
    )?;

    let edge_rows: Vec<(i64, i64, String, Option<String>)> = edges_stmt.query_map(params![project_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,           // caller_id
            row.get::<_, i64>(1)?,           // callee_id
            row.get::<_, String>(2)?,         // edge_type
            row.get::<_, Option<String>>(3)?, // metadata
        ))
    })?.collect::<SqliteResult<Vec<_>>>()?;

    for (caller_id, callee_id, edge_type_str, metadata_json) in edge_rows {
        let caller_node_id = *db_id_to_node_id.get(&caller_id)
            .ok_or_else(|| PdgStoreError::NodeNotFound(caller_id))?;

        let callee_node_id = *db_id_to_node_id.get(&callee_id)
            .ok_or_else(|| PdgStoreError::NodeNotFound(callee_id))?;

        let edge_type = StorageEdgeType::from_str_name(&edge_type_str)
            .ok_or_else(|| PdgStoreError::Deserialization(format!("Invalid edge type: {}", edge_type_str)))?;

        let metadata = match metadata_json.as_deref() {
            Some(json) => serde_json::from_str(json)
                .map_err(|e| PdgStoreError::Deserialization(format!("Invalid edge metadata: {}", e)))?,
            None => StorageEdgeMetadata {
                call_count: None,
                variable_name: None,
            }
        };

        let pdg_edge = PDGEdge {
            edge_type: convert_storage_edge_type(&edge_type),
            metadata: convert_storage_edge_metadata(&metadata),
        };

        pdg.add_edge(caller_node_id, callee_node_id, pdg_edge);
    }

    Ok(pdg)
}

/// Check if a PDG exists for a project
///
/// # Arguments
///
/// * `storage` - Reference to the storage backend
/// * `project_id` - Project identifier to check
///
/// # Returns
///
/// `true` if the project has at least one node, `false` otherwise
pub fn pdg_exists(storage: &Storage, project_id: &str) -> SqliteResult<bool> {
    let count: i64 = storage.conn().query_row(
        "SELECT COUNT(*) FROM intel_nodes WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Delete a PDG from storage
///
/// # Arguments
///
/// * `storage` - Mutable reference to the storage backend
/// * `project_id` - Project identifier to delete
///
/// # Returns
///
/// `Ok(())` if successful, `Err` if an error occurs
pub fn delete_pdg(storage: &mut Storage, project_id: &str) -> SqliteResult<()> {
    // Delete edges first (via subquery to avoid foreign key constraint issues)
    storage.conn().execute(
        "DELETE FROM intel_edges WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
        params![project_id],
    )?;

    // Then delete nodes
    storage.conn().execute(
        "DELETE FROM intel_nodes WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_pdg() -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();

        let n1 = pdg.add_node(PDGNode {
            id: "func1".to_string(),
            node_type: PDGNodeType::Function,
            name: "func1".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            embedding: Some(vec![0.1, 0.2, 0.3]),
        });

        let n2 = pdg.add_node(PDGNode {
            id: "func2".to_string(),
            node_type: PDGNodeType::Function,
            name: "func2".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (100, 200),
            complexity: 3,
            embedding: None,
        });

        pdg.add_edge(n1, n2, PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata {
                call_count: Some(5),
                variable_name: None,
            },
        });

        pdg
    }

    #[test]
    fn test_save_and_load_pdg() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_test_pdg();
        save_pdg(&mut storage, "test_project", &pdg).unwrap();

        assert!(pdg_exists(&storage, "test_project").unwrap());

        let loaded = load_pdg(&storage, "test_project").unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);

        let func1 = loaded.find_by_symbol("func1").unwrap();
        let node1 = loaded.get_node(func1).unwrap();
        assert_eq!(node1.name, "func1");
        assert_eq!(node1.complexity, 5);
        assert_eq!(node1.embedding, Some(vec![0.1, 0.2, 0.3]));
    }

    #[test]
    fn test_save_pdg_replaces_existing() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg1 = create_test_pdg();
        save_pdg(&mut storage, "test_project", &pdg1).unwrap();
        assert_eq!(load_pdg(&storage, "test_project").unwrap().node_count(), 2);

        let mut pdg2 = ProgramDependenceGraph::new();
        pdg2.add_node(PDGNode {
            id: "new_func".to_string(),
            node_type: PDGNodeType::Function,
            name: "new_func".to_string(),
            file_path: "new.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            embedding: None,
        });

        save_pdg(&mut storage, "test_project", &pdg2).unwrap();
        assert_eq!(load_pdg(&storage, "test_project").unwrap().node_count(), 1);
    }

    #[test]
    fn test_load_nonexistent_project() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        let loaded = load_pdg(&storage, "nonexistent").unwrap();
        assert_eq!(loaded.node_count(), 0);
        assert_eq!(loaded.edge_count(), 0);
    }

    #[test]
    fn test_delete_pdg() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_test_pdg();
        save_pdg(&mut storage, "test_project", &pdg).unwrap();
        assert!(pdg_exists(&storage, "test_project").unwrap());

        delete_pdg(&mut storage, "test_project").unwrap();
        assert!(!pdg_exists(&storage, "test_project").unwrap());
    }

    #[test]
    fn test_convert_node_types() {
        assert_eq!(convert_node_type(&PDGNodeType::Function), StorageNodeType::Function);
        assert_eq!(convert_node_type(&PDGNodeType::Class), StorageNodeType::Class);
        assert_eq!(convert_node_type(&PDGNodeType::Method), StorageNodeType::Method);
        assert_eq!(convert_node_type(&PDGNodeType::Variable), StorageNodeType::Variable);
        assert_eq!(convert_node_type(&PDGNodeType::Module), StorageNodeType::Module);

        assert_eq!(convert_storage_node_type(&StorageNodeType::Function), PDGNodeType::Function);
        assert_eq!(convert_storage_node_type(&StorageNodeType::Class), PDGNodeType::Class);
        assert_eq!(convert_storage_node_type(&StorageNodeType::Method), PDGNodeType::Method);
        assert_eq!(convert_storage_node_type(&StorageNodeType::Variable), PDGNodeType::Variable);
        assert_eq!(convert_storage_node_type(&StorageNodeType::Module), PDGNodeType::Module);
    }

    #[test]
    fn test_convert_edge_types() {
        assert_eq!(convert_edge_type(&PDGEdgeType::Call), StorageEdgeType::Call);
        assert_eq!(convert_edge_type(&PDGEdgeType::DataDependency), StorageEdgeType::DataDependency);
        assert_eq!(convert_edge_type(&PDGEdgeType::Inheritance), StorageEdgeType::Inheritance);
        assert_eq!(convert_edge_type(&PDGEdgeType::Import), StorageEdgeType::Import);

        assert_eq!(convert_storage_edge_type(&StorageEdgeType::Call), PDGEdgeType::Call);
        assert_eq!(convert_storage_edge_type(&StorageEdgeType::DataDependency), PDGEdgeType::DataDependency);
        assert_eq!(convert_storage_edge_type(&StorageEdgeType::Inheritance), PDGEdgeType::Inheritance);
        assert_eq!(convert_storage_edge_type(&StorageEdgeType::Import), PDGEdgeType::Import);
    }

    #[test]
    fn test_edge_metadata_conversion() {
        let pdg_meta = PDGEdgeMetadata {
            call_count: Some(42),
            variable_name: Some("x".to_string()),
        };

        let storage_meta = convert_edge_metadata(&pdg_meta);
        assert_eq!(storage_meta.call_count, Some(42));
        assert_eq!(storage_meta.variable_name, Some("x".to_string()));

        let converted_back = convert_storage_edge_metadata(&storage_meta);
        assert_eq!(converted_back.call_count, Some(42));
        assert_eq!(converted_back.variable_name, Some("x".to_string()));
    }

    #[test]
    fn test_save_pdg_with_all_edge_types() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = ProgramDependenceGraph::new();

        let n1 = pdg.add_node(PDGNode {
            id: "child".to_string(),
            node_type: PDGNodeType::Class,
            name: "Child".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            embedding: None,
        });

        let n2 = pdg.add_node(PDGNode {
            id: "parent".to_string(),
            node_type: PDGNodeType::Class,
            name: "Parent".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (50, 100),
            complexity: 1,
            embedding: None,
        });

        let n3 = pdg.add_node(PDGNode {
            id: "data_user".to_string(),
            node_type: PDGNodeType::Function,
            name: "data_user".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (100, 150),
            complexity: 1,
            embedding: None,
        });

        pdg.add_edge(n1, n2, PDGEdge {
            edge_type: PDGEdgeType::Inheritance,
            metadata: PDGEdgeMetadata {
                call_count: None,
                variable_name: None,
            },
        });

        pdg.add_edge(n3, n1, PDGEdge {
            edge_type: PDGEdgeType::DataDependency,
            metadata: PDGEdgeMetadata {
                call_count: None,
                variable_name: Some("child_instance".to_string()),
            },
        });

        save_pdg(&mut storage, "test_project", &pdg).unwrap();

        let loaded = load_pdg(&storage, "test_project").unwrap();
        assert_eq!(loaded.node_count(), 3);
        assert_eq!(loaded.edge_count(), 2);

        // Verify edges by checking connectivity
        let child_id = loaded.find_by_symbol("Child").unwrap();
        let parent_id = loaded.find_by_symbol("Parent").unwrap();
        let data_user_id = loaded.find_by_symbol("data_user").unwrap();

        // Child should have Parent as neighbor (inheritance)
        let child_neighbors = loaded.neighbors(child_id);
        assert!(child_neighbors.contains(&parent_id));

        // data_user should have Child as neighbor (data dependency)
        let data_user_neighbors = loaded.neighbors(data_user_id);
        assert!(data_user_neighbors.contains(&child_id));
    }

    #[test]
    fn test_embedding_blob_serialization() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = ProgramDependenceGraph::new();

        let embedding = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        pdg.add_node(PDGNode {
            id: "func".to_string(),
            node_type: PDGNodeType::Function,
            name: "func".to_string(),
            file_path: "test.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            embedding: Some(embedding.clone()),
        });

        save_pdg(&mut storage, "test_project", &pdg).unwrap();

        let loaded = load_pdg(&storage, "test_project").unwrap();
        let func_id = loaded.find_by_symbol("func").unwrap();
        let func_node = loaded.get_node(func_id).unwrap();

        assert_eq!(func_node.embedding, Some(embedding));
    }
}
