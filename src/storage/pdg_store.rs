// PDG Persistence Bridge
//
// *Le Pont* (The Bridge) - Converts between legraphe PDG and lestockage records

use crate::graph::pdg::{
    Edge as PDGEdge, EdgeMetadata as PDGEdgeMetadata, EdgeType as PDGEdgeType, Node as PDGNode,
    NodeId, NodeType as PDGNodeType, ProgramDependenceGraph,
};
use crate::graph::trigram::TrigramIndex;
use crate::storage::edges::{EdgeMetadata as StorageEdgeMetadata, EdgeType as StorageEdgeType};
use crate::storage::nodes::NodeType as StorageNodeType;
use crate::storage::schema::Storage;
use rusqlite::{Result as SqliteResult, params};
use std::collections::HashMap;
use std::sync::Arc;

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

/// Convert lestockage NodeType to legraphe NodeType
fn convert_storage_node_type(node_type: &StorageNodeType) -> PDGNodeType {
    match node_type {
        StorageNodeType::Function => PDGNodeType::Function,
        StorageNodeType::Class => PDGNodeType::Class,
        StorageNodeType::Method => PDGNodeType::Method,
        StorageNodeType::Variable => PDGNodeType::Variable,
        StorageNodeType::Module => PDGNodeType::Module,
        StorageNodeType::External => PDGNodeType::External,
        StorageNodeType::DocSection => PDGNodeType::DocSection,
        StorageNodeType::FileSummary => PDGNodeType::FileSummary,
    }
}

/// Convert lestockage EdgeType to legraphe EdgeType
fn convert_storage_edge_type(edge_type: &StorageEdgeType) -> PDGEdgeType {
    match edge_type {
        StorageEdgeType::Call => PDGEdgeType::Call,
        StorageEdgeType::DataDependency => PDGEdgeType::DataDependency,
        StorageEdgeType::Inheritance => PDGEdgeType::Inheritance,
        StorageEdgeType::Import => PDGEdgeType::Import,
        StorageEdgeType::Containment => PDGEdgeType::Containment,
        StorageEdgeType::TypeOf => PDGEdgeType::TypeOf,
        StorageEdgeType::StateTransition => PDGEdgeType::StateTransition,
        StorageEdgeType::CommandArgument => PDGEdgeType::CommandArgument,
        StorageEdgeType::Environment => PDGEdgeType::Environment,
        StorageEdgeType::Stdin => PDGEdgeType::Stdin,
    }
}

/// Convert lestockage EdgeMetadata to legraphe EdgeMetadata
fn convert_storage_edge_metadata(metadata: &StorageEdgeMetadata) -> PDGEdgeMetadata {
    PDGEdgeMetadata {
        call_count: metadata.call_count,
        variable_name: metadata.variable_name.clone(),
        confidence: metadata.confidence,
        channel: metadata.channel.clone(),
        position: metadata.position,
    }
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
    // Edge rows are decoded on a second thread over their own read-only
    // connection while this one reads the nodes; applying them needs the
    // node ids, so that part runs afterwards. Falls back to a single
    // connection for in-memory databases or if the second one cannot open.
    let database = storage
        .conn()
        .path()
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    let (nodes, prefetched_edges) = std::thread::scope(|scope| {
        let edges = database.map(|path| {
            scope.spawn(move || {
                let reader = Storage::open_readonly(&path).ok()?;
                read_edges(reader.conn()).ok()
            })
        });
        let mut pdg = ProgramDependenceGraph::new();
        let nodes = load_nodes(storage, project_id, &mut pdg).map(|ids| (pdg, ids));
        (nodes, edges.and_then(|handle| handle.join().ok().flatten()))
    });
    let (mut pdg, db_id_to_node_id) = nodes?;
    let edges = match prefetched_edges {
        Some(edges) => edges,
        None => read_edges(storage.conn())?,
    };
    apply_edges(&mut pdg, &db_id_to_node_id, edges);

    // Try to load persisted trigram index; fall back to rebuilding from nodes.
    // The trigram index is maintained incrementally via add_node during load,
    // but loading the persisted version is faster for large PDGs.
    match load_trigram_index(storage, project_id) {
        Ok(Some(trigram_idx)) => pdg.set_trigram_index(trigram_idx),
        // Nodes were bulk-loaded without trigrams; build them once.
        _ => pdg.rebuild_trigram_index(),
    }

    Ok(pdg)
}

type RowIdMap = HashMap<i64, NodeId, std::hash::BuildHasherDefault<crate::fast_hash::FastHasher>>;

/// Decode the `intel_nodes` row columns that can fail to deserialize
/// (`file_path`, `node_type`); the remaining columns are plain typed gets.
fn decode_node_row_strings<'a>(row: &'a rusqlite::Row<'_>) -> Result<(&'a str, &'a str)> {
    let file_path = row
        .get_ref(1)?
        .as_str()
        .map_err(|e| PdgStoreError::Deserialization(e.to_string()))?;
    let node_type_str = row
        .get_ref(5)?
        .as_str()
        .map_err(|e| PdgStoreError::Deserialization(e.to_string()))?;
    Ok((file_path, node_type_str))
}

/// Parse a persisted `node_type` string into the storage enum.
fn parse_storage_node_type(node_type_str: &str) -> Result<StorageNodeType> {
    StorageNodeType::from_str_name(node_type_str).ok_or_else(|| {
        PdgStoreError::Deserialization(format!("Invalid node type: {}", node_type_str))
    })
}

/// Count the rows the hydration query will stream, so every per-node map can
/// be pre-sized (rehash-and-grow while streaming ~30k rows showed up in
/// hydration profiles).
fn count_project_nodes(storage: &Storage, project_id: &str) -> usize {
    storage
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM intel_nodes WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as usize)
        .unwrap_or(0)
}

/// One `Arc<str>` per file, shared by all of its nodes.
fn shared_file_arc(
    files: &mut crate::fast_hash::FastMap<String, Arc<str>>,
    file_path: &str,
) -> Arc<str> {
    if let Some(shared) = files.get(file_path) {
        return Arc::clone(shared);
    }
    let shared: Arc<str> = Arc::from(file_path);
    files.insert(file_path.to_string(), Arc::clone(&shared));
    shared
}

/// Decode one `intel_nodes` row into a graph node, alongside whether the row
/// carries a precision (stable) symbol id.
fn decode_pdg_node(
    row: &rusqlite::Row<'_>,
    node_type: StorageNodeType,
    file_path: Arc<str>,
) -> Result<(PDGNode, bool)> {
    let start: Option<i64> = row.get(7)?;
    let end: Option<i64> = row.get(8)?;
    let complexity: Option<i32> = row.get(6)?;
    let precision: i32 = row.get(9)?;
    let pdg_node = PDGNode {
        id: row.get(2)?,
        node_type: convert_storage_node_type(&node_type),
        name: row.get(3)?,
        file_path,
        byte_range: (start.unwrap_or(0) as usize, end.unwrap_or(0) as usize),
        complexity: complexity.unwrap_or(0) as u32,
        language: row.get(4)?,
    };
    let is_precision = precision != 0;
    Ok((pdg_node, is_precision))
}

fn load_nodes(
    storage: &Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
) -> Result<RowIdMap> {
    // Only the columns a graph node needs: `qualified_name`, `content_hash`,
    // `embedding` and `embedding_format` are not part of a `PDGNode`, and rows
    // are streamed straight into the graph rather than collected first.
    let mut nodes_stmt = storage.conn().prepare(
        "SELECT id, file_path, node_id, symbol_name, language, node_type, complexity, byte_range_start, byte_range_end, precision
         FROM intel_nodes WHERE project_id = ?1",
    )?;
    let mut rows = nodes_stmt.query(params![project_id])?;
    let node_count = count_project_nodes(storage, project_id);
    let mut db_id_to_node_id = RowIdMap::with_capacity_and_hasher(node_count, Default::default());
    pdg.reserve_nodes(node_count);
    // One `Arc<str>` per file, shared by all of its nodes.
    let mut files: crate::fast_hash::FastMap<String, Arc<str>> = Default::default();

    while let Some(row) = rows.next()? {
        let db_id: i64 = row.get(0)?;
        let (file_path, node_type_str) = decode_node_row_strings(row)?;
        let node_type = parse_storage_node_type(node_type_str)?;
        let file_path: Arc<str> = shared_file_arc(&mut files, file_path);
        // Externals are graph-level vocabulary, not file content: rows from
        // a pre-2.0.0 index still carry the creating file's path, and taking
        // it verbatim would let `remove_file` delete a shared placeholder
        // other files' edges point at. Canonicalize on read so the legacy
        // fallback converges on the graph-level convention.
        let (pdg_node, precision) = {
            let (mut node, precision) = decode_pdg_node(row, node_type, file_path)?;
            if node.node_type == PDGNodeType::External {
                node.file_path = std::sync::Arc::from(crate::graph::pdg::EXTERNAL_NODE_FILE_PATH);
            }
            (node, precision)
        };
        let stable_id = if precision {
            Some(pdg_node.id.clone())
        } else {
            None
        };
        let node_id = pdg.add_node_without_trigrams(pdg_node);
        if let Some(stable_id) = stable_id {
            pdg.mark_precision_symbol(stable_id);
        }
        db_id_to_node_id.insert(db_id, node_id);
    }

    Ok(db_id_to_node_id)
}

/// What an edge with no metadata serializes to (`StorageEdgeMetadata` with every
/// field `None`); `load_edges` recognises it without parsing.
const EMPTY_EDGE_METADATA_JSON: &[u8] =
    br#"{"call_count":null,"variable_name":null,"confidence":null,"channel":null,"position":null}"#;

/// Every edge row, decoded into graph edges. No join against `intel_nodes`:
/// an edge belongs to the project iff both of its endpoints do, which
/// [`apply_edges`] checks against the loaded node ids. (The double self-join
/// cost ~150 ms of the load.)
fn read_edges(conn: &rusqlite::Connection) -> Result<Vec<(i64, i64, PDGEdge)>> {
    let mut edges_stmt =
        conn.prepare("SELECT caller_id, callee_id, edge_type, metadata FROM intel_edges")?;
    let mut rows = edges_stmt.query([])?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next()? {
        let caller_id: i64 = row.get(0)?;
        let callee_id: i64 = row.get(1)?;
        let edge_type_str = row
            .get_ref(2)?
            .as_str()
            .map_err(|e| PdgStoreError::Deserialization(e.to_string()))?;
        let edge_type = StorageEdgeType::from_str_name(edge_type_str).ok_or_else(|| {
            PdgStoreError::Deserialization(format!("Invalid edge type: {}", edge_type_str))
        })?;
        let metadata = match row.get_ref(3)? {
            // ~70% of edges carry the all-null literal; skip the JSON parse.
            rusqlite::types::ValueRef::Text(bytes) if bytes != EMPTY_EDGE_METADATA_JSON => {
                serde_json::from_slice(bytes).map_err(|e| {
                    PdgStoreError::Deserialization(format!("Invalid edge metadata: {}", e))
                })?
            }
            _ => StorageEdgeMetadata {
                call_count: None,
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        };
        edges.push((
            caller_id,
            callee_id,
            PDGEdge {
                edge_type: convert_storage_edge_type(&edge_type),
                metadata: convert_storage_edge_metadata(&metadata),
            },
        ));
    }
    Ok(edges)
}

/// Add the edges whose endpoints are both in `db_id_to_node_id`.
fn apply_edges(
    pdg: &mut ProgramDependenceGraph,
    db_id_to_node_id: &RowIdMap,
    edges: Vec<(i64, i64, PDGEdge)>,
) {
    for (caller_id, callee_id, edge) in edges {
        if let (Some(&caller), Some(&callee)) = (
            db_id_to_node_id.get(&caller_id),
            db_id_to_node_id.get(&callee_id),
        ) {
            pdg.add_edge(caller, callee, edge);
        }
    }
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

/// Remove one file's freshness record. Post-D5 the graph no longer persists
/// as `intel_nodes`/`intel_edges` rows, so there is nothing else to delete —
/// the graph state for a removed file is dropped from the in-memory PDG by
/// the caller and the next published generation encodes the change.
pub fn delete_file_data(
    storage: &mut Storage,
    project_id: &str,
    file_path: &str,
) -> SqliteResult<()> {
    storage.conn().execute(
        "DELETE FROM indexed_files WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;
    Ok(())
}

/// Delete the freshness record for one file within an existing transaction.
/// Post-D5 the graph no longer persists as rows, so only `indexed_files`
/// needs deleting (see `delete_file_data`).
pub fn delete_file_data_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_path: &str,
) -> SqliteResult<()> {
    tx.execute(
        "DELETE FROM indexed_files WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;
    Ok(())
}

/// Delete nodes and edges for multiple files in a single transaction
pub fn delete_files_data_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_paths: &[String],
) -> SqliteResult<()> {
    for file_path in file_paths {
        delete_file_data_tx(tx, project_id, file_path)?;
    }
    Ok(())
}

/// Get all indexed files for a project with their hashes
pub fn get_indexed_files(
    storage: &Storage,
    project_id: &str,
) -> SqliteResult<HashMap<String, String>> {
    let mut stmt = storage
        .conn()
        .prepare("SELECT file_path, file_hash FROM indexed_files WHERE project_id = ?1")?;

    let rows = stmt.query_map(params![project_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut result = HashMap::new();
    for row in rows {
        let (path, hash) = row?;
        result.insert(path, hash);
    }

    Ok(result)
}

/// Check if any indexed files exist for a project (lightweight query)
pub fn has_indexed_files(storage: &Storage, project_id: &str) -> bool {
    storage
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM indexed_files WHERE project_id = ?1 LIMIT 1",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0
}

/// Update indexed file record
pub fn update_indexed_file(
    storage: &mut Storage,
    project_id: &str,
    file_path: &str,
    hash: &str,
) -> SqliteResult<()> {
    storage.conn().execute(
        "INSERT INTO indexed_files (file_path, project_id, file_hash, last_indexed)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(file_path) DO UPDATE SET file_hash = ?3, last_indexed = ?4",
        params![file_path, project_id, hash, chrono::Utc::now().timestamp()],
    )?;
    Ok(())
}

/// Update multiple indexed files within a single transaction
pub fn update_indexed_files_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    files: &[(String, String)], // (file_path, file_hash)
) -> SqliteResult<()> {
    let now = chrono::Utc::now().timestamp();
    for (file_path, file_hash) in files {
        tx.execute(
            "INSERT INTO indexed_files (file_path, project_id, file_hash, last_indexed)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(file_path) DO UPDATE SET file_hash = ?3, last_indexed = ?4",
            params![file_path, project_id, file_hash, now],
        )?;
    }
    Ok(())
}

/// Load the trigram index for a project from storage.
///
/// Returns `Ok(Some(TrigramIndex))` if a persisted index exists,
/// `Ok(None)` if no index has been saved yet.
pub fn load_trigram_index(storage: &Storage, project_id: &str) -> Result<Option<TrigramIndex>> {
    let result = storage.conn().query_row(
        "SELECT index_data FROM trigram_index WHERE project_id = ?1",
        params![project_id],
        |row| row.get::<_, Vec<u8>>(0),
    );

    match result {
        Ok(data) => {
            let index = TrigramIndex::deserialize(&data).ok_or_else(|| {
                PdgStoreError::Deserialization("Failed to deserialize trigram index".to_string())
            })?;
            Ok(Some(index))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
#[path = "pdg_store_test.rs"]
mod tests;
