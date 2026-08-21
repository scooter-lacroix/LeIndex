// PDG Persistence Bridge
//
// *Le Pont* (The Bridge) - Converts between legraphe PDG and lestockage records

use crate::graph::pdg::{
    Edge as PDGEdge, EdgeMetadata as PDGEdgeMetadata, EdgeType as PDGEdgeType, Node as PDGNode,
    NodeId, NodeType as PDGNodeType, ProgramDependenceGraph,
};
use crate::graph::trigram::TrigramIndex;
use crate::storage::edges::{EdgeMetadata as StorageEdgeMetadata, EdgeType as StorageEdgeType};
use crate::storage::nodes::{NodeRecord, NodeType as StorageNodeType};
use crate::storage::schema::Storage;
use rusqlite::types::Value;
use rusqlite::{Result as SqliteResult, params};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Type alias for node database rows to reduce type complexity
type NodeDbRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<i32>,
    String,
    Option<Vec<u8>>,
    Option<i64>,
    Option<i64>,
    Option<i32>,
    i32,
);

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

/// Maximum rows written per multi-row INSERT in `save_nodes` / `save_edges`.
///
/// Sized so a single statement stays well below SQLite's default
/// `SQLITE_MAX_VARIABLE_NUMBER` (32766): 500 nodes × 16 columns = 8000 bound
/// variables. For 10K / 50K nodes this lowers statement count from ~10K / 50K
/// down to ~20 / 100.
const PDG_INSERT_BATCH_SIZE: usize = 500;

/// Maximum full-transaction retries when SQLite reports a transient
/// busy/locked condition. The MCP server can hold the same database open on
/// several connections (writer + reader pool + catalog readers); a competing
/// lock is a normal condition, not a corruption, so a bounded retry with
/// backoff turns intermittent "database is locked" failures into successful
/// saves instead of failed index generations.
const SAVE_PDG_MAX_RETRIES: u32 = 3;

/// Base delay (ms) before the first retry; each retry multiplies it.
const SAVE_PDG_RETRY_BASE_DELAY_MS: u64 = 100;

/// WAL auto-checkpoint threshold (pages) re-asserted before every bulk save.
/// 1000 pages ≈ 4 MiB; keeping the WAL near this size bounds checkpoint
/// latency and prevents unbounded WAL growth on long-lived servers.
const SAVE_PDG_WAL_AUTOCHECKPOINT_PAGES: i64 = 1000;

/// Re-assert connection-level pragmas required for a safe bulk write. These
/// are normally set by `Storage::open_with_config`, but legacy databases
/// (opened before WAL became the default) and connections created through
/// non-standard configs may lack them; re-asserting is a no-op when already
/// active and runs outside any transaction.
fn ensure_write_connection_pragmas(storage: &mut Storage) -> Result<()> {
    storage
        .conn_mut()
        .pragma_update(None, "journal_mode", "WAL")?;
    storage
        .conn_mut()
        .pragma_update(None, "synchronous", "NORMAL")?;
    // Wait up to 5s for a competing writer instead of failing immediately.
    storage
        .conn_mut()
        .pragma_update(None, "busy_timeout", 5000)?;
    // Bound WAL growth so a long-lived MCP server does not accumulate a
    // multi-hundred-MB WAL between checkpoints.
    storage.conn_mut().pragma_update(
        None,
        "wal_autocheckpoint",
        SAVE_PDG_WAL_AUTOCHECKPOINT_PAGES,
    )?;
    // Best-effort passive checkpoint: trims the WAL before the bulk write
    // grows it again. Passive never blocks readers or other writers; if the
    // WAL is busy it simply returns SQLITE_BUSY, which we ignore here.
    let _ = storage
        .conn_mut()
        .execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
    Ok(())
}

/// Whether a rusqlite error represents a transient lock condition that a
/// bounded retry can plausibly clear.
fn is_transient_lock_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(ferror, _)
            if matches!(
                ferror.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Convert legraphe NodeType to lestockage NodeType
fn convert_node_type(node_type: &PDGNodeType) -> StorageNodeType {
    match node_type {
        PDGNodeType::Function => StorageNodeType::Function,
        PDGNodeType::Class => StorageNodeType::Class,
        PDGNodeType::Method => StorageNodeType::Method,
        PDGNodeType::Variable => StorageNodeType::Variable,
        PDGNodeType::Module => StorageNodeType::Module,
        PDGNodeType::External => StorageNodeType::External,
        PDGNodeType::DocSection => StorageNodeType::DocSection,
        PDGNodeType::FileSummary => StorageNodeType::FileSummary,
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
        StorageNodeType::External => PDGNodeType::External,
        StorageNodeType::DocSection => PDGNodeType::DocSection,
        StorageNodeType::FileSummary => PDGNodeType::FileSummary,
    }
}

/// Convert legraphe EdgeType to lestockage EdgeType
fn convert_edge_type(edge_type: &PDGEdgeType) -> StorageEdgeType {
    match edge_type {
        PDGEdgeType::Call => StorageEdgeType::Call,
        PDGEdgeType::DataDependency => StorageEdgeType::DataDependency,
        PDGEdgeType::Inheritance => StorageEdgeType::Inheritance,
        PDGEdgeType::Import => StorageEdgeType::Import,
        PDGEdgeType::Containment => StorageEdgeType::Containment,
        PDGEdgeType::TypeOf => StorageEdgeType::TypeOf,
        PDGEdgeType::StateTransition => StorageEdgeType::StateTransition,
        PDGEdgeType::CommandArgument => StorageEdgeType::CommandArgument,
        PDGEdgeType::Environment => StorageEdgeType::Environment,
        PDGEdgeType::Stdin => StorageEdgeType::Stdin,
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

/// Convert legraphe EdgeMetadata to lestockage EdgeMetadata
fn convert_edge_metadata(metadata: &PDGEdgeMetadata) -> StorageEdgeMetadata {
    StorageEdgeMetadata {
        call_count: metadata.call_count,
        variable_name: metadata.variable_name.clone(),
        confidence: metadata.confidence,
        channel: metadata.channel.clone(),
        position: metadata.position,
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
    ensure_write_connection_pragmas(storage)?;

    // Retry the whole transaction on transient lock contention. Each attempt
    // rebuilds the transaction from scratch, so a partially executed attempt
    // is rolled back and never leaks partial state.
    let mut attempt = 0u32;
    loop {
        match save_pdg_inner(storage, project_id, pdg) {
            Ok(()) => return Ok(()),
            Err(PdgStoreError::Sqlite(error))
                if is_transient_lock_error(&error) && attempt < SAVE_PDG_MAX_RETRIES =>
            {
                attempt += 1;
                tracing::warn!(
                    project = project_id,
                    attempt,
                    error = %error,
                    "save_pdg blocked by a competing connection; retrying"
                );
                std::thread::sleep(std::time::Duration::from_millis(
                    SAVE_PDG_RETRY_BASE_DELAY_MS * u64::from(attempt),
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Single non-retrying attempt at persisting a PDG. See [`save_pdg`].
fn save_pdg_inner(
    storage: &mut Storage,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<()> {
    let started = std::time::Instant::now();
    // BEGIN IMMEDIATE, not the default deferred BEGIN: the diff reads rows
    // before writing, and SQLite refuses a read→write transaction upgrade
    // with an open read snapshot by returning SQLITE_BUSY IMMEDIATELY — the
    // busy_timeout is not consulted, so every retry would fail instantly
    // while a competing writer (external rebuild, another MCP server) holds
    // the lock. Taking the write lock upfront makes busy_timeout apply and
    // the bounded retry loop actually able to wait the writer out. The
    // previous delete-first ordering achieved the same effect by accident.
    let tx = storage
        .conn_mut()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Nodes are diffed against the persisted rows (unchanged nodes reuse their
    // db ids and issue no write); edges are diffed in save_edges the same way.
    // Edge deletion happens INSIDE save_edges so the edges of stale nodes are
    // gone before the nodes themselves are deleted (FK-safe ordering).
    let (node_id_map, stale_node_ids) = save_nodes(&tx, project_id, pdg)?;
    let nodes_elapsed = started.elapsed();

    let edge_stats = save_edges(&tx, project_id, &node_id_map, pdg)?;
    let edges_elapsed = started.elapsed();

    // Remove nodes that are no longer part of the PDG. Their edges were
    // already removed by the edge diff above. The stale set is computed in
    // Rust against the pre-query, so each DELETE stays well under
    // SQLITE_MAX_VARIABLE_NUMBER.
    for chunk in stale_node_ids.chunks(PDG_INSERT_BATCH_SIZE) {
        let placeholders = (0..chunk.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "DELETE FROM intel_nodes WHERE project_id = ?1 AND node_id IN ({placeholders})"
        );
        let mut params: Vec<rusqlite::types::Value> = vec![Value::Text(project_id.to_string())];
        params.extend(chunk.iter().map(|node_id| node_id.clone().into()));
        tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
    }

    // Save trigram index alongside the PDG (within the same transaction)
    if let Err(e) = save_trigram_index_tx(&tx, project_id, pdg.trigram_index()) {
        // Log but don't fail — the trigram index is a performance optimization,
        // not a correctness requirement. It will be rebuilt on load if missing.
        tracing::warn!("Failed to save trigram index: {e}");
    }

    tx.commit()?;
    tracing::info!(
        project = project_id,
        total_ms = started.elapsed().as_millis() as u64,
        nodes_until_ms = nodes_elapsed.as_millis() as u64,
        edges_until_ms = edges_elapsed.as_millis() as u64,
        pdg_nodes = pdg.node_count(),
        pdg_edges = pdg.edge_count(),
        edges_written = edge_stats.written,
        edges_deleted = edge_stats.deleted,
        edges_skipped = pdg.edge_count().saturating_sub(edge_stats.written),
        "save_pdg diff summary"
    );
    Ok(())
}

/// Write accounting for one `save_edges` pass, used for logging and tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct EdgeSaveStats {
    written: usize,
    deleted: usize,
}

/// Content hash for a node — computed once per node and reused for both the
/// persisted `content_hash` column and the unchanged-row skip check (C3).
///
/// Hashes the content-bearing fields only. `node_id` is deliberately excluded:
/// it is the row identity / upsert conflict key, so a changed id is a new row,
/// not an update. The 0x1f separator cannot appear in any field (paths and
/// identifiers cannot contain it; complexity and byte ranges are numeric).
fn node_content_hash(
    file_path: &str,
    symbol_name: &str,
    qualified_name: &str,
    language: &str,
    node_type: &StorageNodeType,
    complexity: u32,
    byte_range: (usize, usize),
) -> String {
    let mut hasher = blake3::Hasher::new();
    for field in [
        file_path.as_bytes(),
        symbol_name.as_bytes(),
        qualified_name.as_bytes(),
        language.as_bytes(),
        node_type.as_str().as_bytes(),
    ] {
        hasher.update(field);
        hasher.update(&[0x1f]);
    }
    hasher.update(&complexity.to_le_bytes());
    hasher.update(&[0x1f]);
    hasher.update(&byte_range.0.to_le_bytes());
    hasher.update(&[0x1f]);
    hasher.update(&byte_range.1.to_le_bytes());
    hasher.finalize().to_hex().to_string()
}

fn save_nodes(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<(HashMap<NodeId, i64>, Vec<String>)> {
    // Pre-query existing rows for the project so an unchanged node reuses its
    // db id and issues no write (C1). A legacy row whose content_hash was
    // computed as blake3(node_id) under the old scheme reads as changed once
    // and is rewritten under the new scheme on the first save.
    let mut existing: HashMap<String, (i64, String, bool)> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id, node_id, content_hash, precision FROM intel_nodes WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i32>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (db_id, node_id, content_hash, precision) = row?;
            existing.insert(node_id, (db_id, content_hash, precision));
        }
    }

    let node_indices: Vec<NodeId> = pdg.node_indices().collect();
    let mut node_id_map: HashMap<NodeId, i64> = HashMap::with_capacity(node_indices.len());
    let mut to_upsert: Vec<(usize, NodeRecord)> = Vec::with_capacity(node_indices.len());

    for (pos, &node_idx) in node_indices.iter().enumerate() {
        let pdg_node = pdg
            .get_node(node_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Missing node data".to_string()))?;

        let qualified_name = pdg_node
            .id
            .split(':')
            .next_back()
            .unwrap_or(&pdg_node.id)
            .to_string();
        // C3: one hash per node, reused for the column and the skip check.
        let content_hash = node_content_hash(
            &pdg_node.file_path,
            &pdg_node.name,
            &qualified_name,
            &pdg_node.language,
            &convert_node_type(&pdg_node.node_type),
            pdg_node.complexity,
            pdg_node.byte_range,
        );

        let graph_precision = pdg.is_precision_symbol(&pdg_node.id);
        if let Some((db_id, stored_hash, stored_precision)) = existing.get(&pdg_node.id) {
            if *stored_hash == content_hash && *stored_precision == graph_precision {
                // Unchanged node: reuse the existing row, issue no write.
                node_id_map.insert(node_idx, *db_id);
                continue;
            }
        }

        // Note: Embeddings are externalized to EmbeddingStore, not stored here.
        let record = NodeRecord {
            id: None,
            project_id: project_id.to_string(),
            file_path: pdg_node.file_path.to_string(),
            node_id: pdg_node.id.clone(),
            symbol_name: pdg_node.name.clone(),
            qualified_name,
            language: pdg_node.language.clone(),
            node_type: convert_node_type(&pdg_node.node_type),
            signature: None, // Could be populated from node content
            complexity: Some(pdg_node.complexity as i32),
            content_hash,
            embedding: None, // Embeddings externalized to EmbeddingStore
            byte_range_start: Some(pdg_node.byte_range.0 as i64),
            byte_range_end: Some(pdg_node.byte_range.1 as i64),
            embedding_format: Some(0),
            precision: graph_precision,
        };
        to_upsert.push((pos, record));
    }

    // Stale = rows present in the DB but absent from the new PDG.
    let keep: HashSet<&str> = node_indices
        .iter()
        .filter_map(|&idx| pdg.get_node(idx))
        .map(|node| node.id.as_str())
        .collect();
    let stale_node_ids: Vec<String> = existing
        .keys()
        .filter(|node_id| !keep.contains(node_id.as_str()))
        .cloned()
        .collect();

    // Upsert only new/changed nodes in batches. `RETURNING id` emits rows in
    // the same order as the VALUES tuples, so the returned db ids map 1:1 onto
    // the chunk's (pos, record) pairs. The invariant that makes this mapping
    // sound is that the statement returns EXACTLY one row per input tuple.
    //
    // A previous `DO UPDATE ... WHERE content_hash != excluded.content_hash`
    // guard violated that invariant: when a chunk contains duplicate
    // `node_id`s (the graph legitimately holds same-named symbols from
    // different files), the second tuple's guard evaluated false, suppressed
    // the UPDATE, and `RETURNING` returned fewer rows than the chunk. The
    // `zip` below then silently dropped the tail nodes from `node_id_map`,
    // so their edges referenced non-existent rows and the whole persist
    // failed with `EdgeNodeMissing`. Unchanged rows are already filtered out
    // by the Rust-side content_hash comparison above, so the guard was both
    // redundant and harmful; it is removed here.
    for chunk in to_upsert.chunks(PDG_INSERT_BATCH_SIZE) {
        let n = chunk.len();

        // Each row carries its own set of 17 anonymous `?` placeholders, bound
        // positionally so they align with `params` below.
        let values_clause = (0..n)
            .map(|_| "(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO intel_nodes \
             (project_id, file_path, node_id, symbol_name, qualified_name, \
              language, node_type, signature, complexity, content_hash, embedding, \
              byte_range_start, byte_range_end, created_at, updated_at, embedding_format, precision) \
             VALUES {values_clause} \
             ON CONFLICT(project_id, node_id) DO UPDATE SET \
               file_path = excluded.file_path, \
               symbol_name = excluded.symbol_name, \
               qualified_name = excluded.qualified_name, \
               language = excluded.language, \
               node_type = excluded.node_type, \
               signature = excluded.signature, \
               complexity = excluded.complexity, \
               content_hash = excluded.content_hash, \
               embedding = excluded.embedding, \
               byte_range_start = excluded.byte_range_start, \
               byte_range_end = excluded.byte_range_end, \
               embedding_format = excluded.embedding_format, \
               precision = excluded.precision, \
               updated_at = excluded.updated_at \
             RETURNING id"
        );

        let now = chrono::Utc::now().timestamp();
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(n * 17);
        for (_, record) in chunk {
            params.push(record.project_id.clone().into());
            params.push(record.file_path.clone().into());
            params.push(record.node_id.clone().into());
            params.push(record.symbol_name.clone().into());
            params.push(record.qualified_name.clone().into());
            params.push(record.language.clone().into());
            params.push(record.node_type.as_str().to_string().into());
            params.push(
                record
                    .signature
                    .clone()
                    .map(Value::Text)
                    .unwrap_or(Value::Null),
            );
            params.push(
                record
                    .complexity
                    .map(|c| Value::Integer(c.into()))
                    .unwrap_or(Value::Null),
            );
            params.push(record.content_hash.clone().into());
            params.push(
                record
                    .embedding
                    .clone()
                    .map(Value::Blob)
                    .unwrap_or(Value::Null),
            );
            params.push(
                record
                    .byte_range_start
                    .map(Value::Integer)
                    .unwrap_or(Value::Null),
            );
            params.push(
                record
                    .byte_range_end
                    .map(Value::Integer)
                    .unwrap_or(Value::Null),
            );
            params.push(Value::Integer(now));
            params.push(Value::Integer(now));
            params.push(
                record
                    .embedding_format
                    .map(|f| Value::Integer(f.into()))
                    .unwrap_or(Value::Null),
            );
            params.push(Value::Integer(record.precision as i64));
        }

        let mut stmt = tx.prepare(&sql)?;
        let ids: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // The statement now returns exactly one row per tuple. If this ever
        // regresses (e.g. a future conditional guard), fail loudly rather than
        // silently dropping nodes from the map and corrupting edge references.
        if ids.len() != chunk.len() {
            return Err(PdgStoreError::Serialization(format!(
                "node upsert returned {} ids for {} tuples; refusing to save a partial node map",
                ids.len(),
                chunk.len(),
            )));
        }
        for (&(pos, _), db_id) in chunk.iter().zip(ids) {
            node_id_map.insert(node_indices[pos], db_id);
        }
    }

    Ok((node_id_map, stale_node_ids))
}

/// Persisted-row identity of an edge: the `intel_edges` primary key.
type EdgeKey = (i64, i64, String);

/// Persist edges by DIFFING against the rows already stored for the project,
/// mirroring the node content-hash skip: an edge whose (caller, callee, type)
/// row already exists with identical metadata JSON issues no write at all.
///
/// This replaces the previous unconditional delete-all + reinsert, which
/// rewrote every edge row (110K+ on large projects) on every save, including
/// one-file incremental deltas where nothing changed.
///
/// Semantics preserved from the old implementation:
/// - duplicate parallel edges with the same PK collapse last-wins (the desired
///   map's `insert` overwrites, matching "later INSERT wins" upsert order);
/// - rows whose caller node belongs to another project are never touched (the
///   existing-rows query filters by caller-side project membership, exactly
///   like the old bulk DELETE did).
fn save_edges(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    node_id_map: &HashMap<NodeId, i64>,
    pdg: &ProgramDependenceGraph,
) -> Result<EdgeSaveStats> {
    // Existing rows for the project: one sequential read (no WAL growth)
    // instead of a full-table rewrite. NULL metadata (legacy rows) reads as
    // the empty string, which never equals serialized JSON, so such rows are
    // rewritten once and converge.
    let mut existing: HashMap<EdgeKey, String> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT e.caller_id, e.callee_id, e.edge_type, e.metadata
             FROM intel_edges e
             WHERE e.caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ))
        })?;
        for row in rows {
            let (caller, callee, edge_type, metadata) = row?;
            existing.insert((caller, callee, edge_type), metadata);
        }
    }

    let mut desired: HashMap<EdgeKey, String> = HashMap::with_capacity(pdg.edge_count());
    for edge_idx in pdg.edge_indices() {
        let (source, target) = pdg
            .edge_endpoints(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Edge has no endpoints".to_string()))?;
        let pdg_edge = pdg
            .get_edge(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Missing edge data".to_string()))?;
        let caller_id =
            *node_id_map
                .get(&source)
                .ok_or_else(|| PdgStoreError::EdgeNodeMissing {
                    caller: source.index() as i64,
                    callee: target.index() as i64,
                })?;
        let callee_id =
            *node_id_map
                .get(&target)
                .ok_or_else(|| PdgStoreError::EdgeNodeMissing {
                    caller: source.index() as i64,
                    callee: target.index() as i64,
                })?;
        let metadata = convert_edge_metadata(&pdg_edge.metadata);
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|e| PdgStoreError::Serialization(e.to_string()))?;
        desired.insert(
            (
                caller_id,
                callee_id,
                convert_edge_type(&pdg_edge.edge_type).as_str().to_string(),
            ),
            metadata_json,
        );
    }

    // Stale = persisted rows absent from the desired set (includes every edge
    // of a removed node, since those keys cannot be produced from the current
    // node_id_map).
    let stale: Vec<EdgeKey> = existing
        .keys()
        .filter(|key| !desired.contains_key(key))
        .cloned()
        .collect();

    let mut stats = EdgeSaveStats::default();

    // When most of the table churns (major refactor / different content), the
    // single subquery DELETE beats thousands of parameterized OR clauses and
    // every desired edge becomes a fresh insert — i.e. the old full-rebuild
    // path. Otherwise delete exactly the stale rows via their primary key.
    let bulk = existing.len() >= 64 && stale.len() * 2 > existing.len();
    if stale.is_empty() {
        // Nothing to delete.
    } else if bulk {
        tx.execute(
            "DELETE FROM intel_edges WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
            params![project_id],
        )?;
        stats.deleted = stale.len();
    } else {
        // Row-value IN over a VALUES table: flat (no expression-tree depth
        // growth — a chain of ORs nests left-associatively and blows
        // SQLITE_LIMIT_EXPR_DEPTH=1000 at ~1000 rows), and the planner can
        // use the (caller_id, callee_id, edge_type) PK index for the probe.
        // 3 bound params per row; 1000 rows = 3000 params, well under
        // SQLITE_MAX_VARIABLE_NUMBER (32766).
        const EDGE_DELETE_CHUNK: usize = 1000;
        for chunk in stale.chunks(EDGE_DELETE_CHUNK) {
            let values = (0..chunk.len())
                .map(|_| "(?,?,?)")
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "DELETE FROM intel_edges WHERE (caller_id, callee_id, edge_type) IN (VALUES {values})"
            );
            let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(chunk.len() * 3);
            for (caller, callee, edge_type) in chunk {
                params.push(Value::Integer(*caller));
                params.push(Value::Integer(*callee));
                params.push(Value::Text(edge_type.clone()));
            }
            tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
            stats.deleted += chunk.len();
        }
    }

    // After a bulk delete every desired edge is a fresh insert; otherwise only
    // new rows and rows whose metadata JSON changed are written (the upsert's
    // DO UPDATE arm covers changed metadata).
    let mut to_write: Vec<(i64, i64, String, String)> = Vec::new();
    for (key, metadata) in &desired {
        let unchanged = !bulk && existing.get(key).is_some_and(|stored| stored == metadata);
        if !unchanged {
            to_write.push((key.0, key.1, key.2.clone(), metadata.clone()));
        }
    }

    // Batch inserts into multi-row statements, mirroring the node batching.
    for chunk in to_write.chunks(PDG_INSERT_BATCH_SIZE) {
        let n = chunk.len();
        let values_clause = (0..n).map(|_| "(?,?,?,?)").collect::<Vec<_>>().join(", ");
        let sql = format!(
            "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata) \
             VALUES {values_clause} \
             ON CONFLICT DO UPDATE SET metadata = excluded.metadata"
        );

        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(n * 4);
        for (caller_id, callee_id, edge_type, metadata_json) in chunk {
            params.push(Value::Integer(*caller_id));
            params.push(Value::Integer(*callee_id));
            params.push(Value::Text(edge_type.clone()));
            params.push(Value::Text(metadata_json.clone()));
        }

        tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        stats.written += n;
    }

    Ok(stats)
}

/// Save trigram index within an existing transaction.
///
/// Hash-skip: the serialized index is a pure function of the PDG's node set,
/// so when the stored content hash matches, the multi-MB blob rewrite is
/// skipped entirely (no-op re-saves and unchanged-graph watcher passes pay
/// only the serialize + blake3 cost, not the write).
fn save_trigram_index_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    trigram_index: &TrigramIndex,
) -> SqliteResult<()> {
    let serialized = trigram_index.serialize();
    let node_count = trigram_index.node_count() as i64;
    let trigram_count = trigram_index.trigram_count() as i64;

    let mut hasher = blake3::Hasher::new();
    hasher.update(&serialized);
    let content_hash = hasher.finalize().to_hex().to_string();

    let stored: Option<String> = tx
        .query_row(
            "SELECT content_hash FROM trigram_index WHERE project_id = ?1",
            params![project_id],
            |row| row.get::<_, String>(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if stored.as_deref() == Some(content_hash.as_str()) {
        return Ok(());
    }

    tx.execute(
        "INSERT INTO trigram_index (project_id, index_data, node_count, trigram_count, updated_at, content_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(project_id) DO UPDATE SET
            index_data = excluded.index_data,
            node_count = excluded.node_count,
            trigram_count = excluded.trigram_count,
            updated_at = excluded.updated_at,
            content_hash = excluded.content_hash",
        params![
            project_id,
            serialized,
            node_count,
            trigram_count,
            chrono::Utc::now().timestamp(),
            content_hash,
        ],
    )?;

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
    let db_id_to_node_id = load_nodes(storage, project_id, &mut pdg)?;
    load_edges(storage, project_id, &mut pdg, &db_id_to_node_id)?;

    // Try to load persisted trigram index; fall back to rebuilding from nodes.
    // The trigram index is maintained incrementally via add_node during load,
    // but loading the persisted version is faster for large PDGs.
    if let Ok(Some(trigram_idx)) = load_trigram_index(storage, project_id) {
        pdg.set_trigram_index(trigram_idx);
    }
    // If no persisted index, the one built incrementally via add_node is already correct.

    Ok(pdg)
}

fn load_nodes(
    storage: &Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
) -> Result<HashMap<i64, NodeId>> {
    let mut nodes_stmt = storage.conn().prepare(
        "SELECT id, file_path, node_id, symbol_name, qualified_name, language, node_type, complexity, content_hash, embedding, byte_range_start, byte_range_end, embedding_format, precision
         FROM intel_nodes WHERE project_id = ?1",
    )?;
    let node_rows: Vec<NodeDbRow> = nodes_stmt
        .query_map(params![project_id], read_node_row)?
        .collect::<SqliteResult<Vec<_>>>()?;
    let mut db_id_to_node_id = HashMap::new();

    for (
        db_id,
        file_path,
        node_id_str,
        symbol_name,
        _qualified_name,
        language,
        node_type_str,
        complexity,
        _content_hash,
        _embedding_blob,
        start,
        end,
        _embedding_format,
        precision,
    ) in node_rows
    {
        let node_type = StorageNodeType::from_str_name(&node_type_str).ok_or_else(|| {
            PdgStoreError::Deserialization(format!("Invalid node type: {}", node_type_str))
        })?;
        let pdg_node = PDGNode {
            id: node_id_str,
            node_type: convert_storage_node_type(&node_type),
            name: symbol_name,
            file_path: Arc::from(file_path),
            byte_range: (start.unwrap_or(0) as usize, end.unwrap_or(0) as usize),
            complexity: complexity.unwrap_or(0) as u32,
            language,
        };
        let stable_id = pdg_node.id.clone();
        let node_id = pdg.add_node(pdg_node);
        if precision != 0 {
            pdg.mark_precision_symbol(stable_id);
        }
        db_id_to_node_id.insert(db_id, node_id);
    }

    Ok(db_id_to_node_id)
}

fn read_node_row(row: &rusqlite::Row<'_>) -> SqliteResult<NodeDbRow> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, String>(3)?,
        row.get::<_, String>(4)?,
        row.get::<_, String>(5)?,
        row.get::<_, String>(6)?,
        row.get::<_, Option<i32>>(7)?,
        row.get::<_, String>(8)?,
        row.get::<_, Option<Vec<u8>>>(9)?,
        row.get::<_, Option<i64>>(10)?,
        row.get::<_, Option<i64>>(11)?,
        row.get::<_, Option<i32>>(12)?,
        row.get::<_, i32>(13)?,
    ))
}

fn load_edges(
    storage: &Storage,
    project_id: &str,
    pdg: &mut ProgramDependenceGraph,
    db_id_to_node_id: &HashMap<i64, NodeId>,
) -> Result<()> {
    let mut edges_stmt = storage.conn().prepare(
        "SELECT e.caller_id, e.callee_id, e.edge_type, e.metadata
         FROM intel_edges e
         INNER JOIN intel_nodes n1 ON e.caller_id = n1.id
         INNER JOIN intel_nodes n2 ON e.callee_id = n2.id
         WHERE n1.project_id = ?1 AND n2.project_id = ?1",
    )?;
    let edge_rows: Vec<(i64, i64, String, Option<String>)> = edges_stmt
        .query_map(params![project_id], read_edge_row)?
        .collect::<SqliteResult<Vec<_>>>()?;

    for (caller_id, callee_id, edge_type_str, metadata_json) in edge_rows {
        let caller_node_id = *db_id_to_node_id
            .get(&caller_id)
            .ok_or_else(|| PdgStoreError::NodeNotFound(caller_id))?;
        let callee_node_id = *db_id_to_node_id
            .get(&callee_id)
            .ok_or_else(|| PdgStoreError::NodeNotFound(callee_id))?;
        let edge_type = StorageEdgeType::from_str_name(&edge_type_str).ok_or_else(|| {
            PdgStoreError::Deserialization(format!("Invalid edge type: {}", edge_type_str))
        })?;
        let metadata = match metadata_json.as_deref() {
            Some(json) => serde_json::from_str(json).map_err(|e| {
                PdgStoreError::Deserialization(format!("Invalid edge metadata: {}", e))
            })?,
            None => StorageEdgeMetadata {
                call_count: None,
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        };
        let pdg_edge = PDGEdge {
            edge_type: convert_storage_edge_type(&edge_type),
            metadata: convert_storage_edge_metadata(&metadata),
        };
        pdg.add_edge(caller_node_id, callee_node_id, pdg_edge);
    }

    Ok(())
}

fn read_edge_row(row: &rusqlite::Row<'_>) -> SqliteResult<(i64, i64, String, Option<String>)> {
    Ok((
        row.get::<_, i64>(0)?,
        row.get::<_, i64>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, Option<String>>(3)?,
    ))
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
pub fn delete_pdg(storage: &mut Storage, project_id: &str) -> SqliteResult<()> {
    // Delete edges first
    storage.conn().execute(
        "DELETE FROM intel_edges WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
        params![project_id],
    )?;

    // Then delete nodes
    storage.conn().execute(
        "DELETE FROM intel_nodes WHERE project_id = ?1",
        params![project_id],
    )?;

    // Delete indexed files records
    storage.conn().execute(
        "DELETE FROM indexed_files WHERE project_id = ?1",
        params![project_id],
    )?;

    // Delete trigram index
    if let Err(e) = delete_trigram_index(storage, project_id) {
        tracing::warn!(
            "Failed to delete trigram index for project {}: {e}",
            project_id
        );
    }

    Ok(())
}

/// Delete nodes and edges for a specific file in a project
pub fn delete_file_data(
    storage: &mut Storage,
    project_id: &str,
    file_path: &str,
) -> SqliteResult<()> {
    // Delete edges where caller or callee belongs to this file
    storage.conn().execute(
        "DELETE FROM intel_edges WHERE 
         caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2) OR
         callee_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2)",
        params![project_id, file_path],
    )?;

    // Delete nodes for this file
    storage.conn().execute(
        "DELETE FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;

    // Delete indexed file record
    storage.conn().execute(
        "DELETE FROM indexed_files WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;

    Ok(())
}

/// Delete nodes and edges for a specific file within an existing transaction
pub fn delete_file_data_tx(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    file_path: &str,
) -> SqliteResult<()> {
    tx.execute(
        "DELETE FROM intel_edges WHERE
         caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2) OR
         callee_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2)",
        params![project_id, file_path],
    )?;
    tx.execute(
        "DELETE FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2",
        params![project_id, file_path],
    )?;
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

/// Save the trigram index for a project to storage.
///
/// The trigram index is serialized to a binary blob and stored in the
/// `trigram_index` table. This avoids rebuilding the index on every load.
pub fn save_trigram_index(
    storage: &mut Storage,
    project_id: &str,
    trigram_index: &TrigramIndex,
) -> Result<()> {
    let serialized = trigram_index.serialize();
    let node_count = trigram_index.node_count() as i64;
    let trigram_count = trigram_index.trigram_count() as i64;

    storage.conn().execute(
        "INSERT INTO trigram_index (project_id, index_data, node_count, trigram_count, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id) DO UPDATE SET
            index_data = excluded.index_data,
            node_count = excluded.node_count,
            trigram_count = excluded.trigram_count,
            updated_at = excluded.updated_at",
        params![
            project_id,
            serialized,
            node_count,
            trigram_count,
            chrono::Utc::now().timestamp(),
        ],
    )?;

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

/// Delete the trigram index for a project.
pub fn delete_trigram_index(storage: &mut Storage, project_id: &str) -> SqliteResult<()> {
    storage.conn().execute(
        "DELETE FROM trigram_index WHERE project_id = ?1",
        params![project_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Storage;
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::NamedTempFile;

    /// Counts of INSERT/DELETE statements emitted against intel_nodes /
    /// intel_edges, populated by a raw `sqlite3_trace` callback installed for
    /// the batch tests below. Statics (not a captured closure) because the
    /// trace hook is an `unsafe extern "C"` function pointer and cannot
    /// capture state. Note: legacy sqlite3_trace delivers SQL with bound
    /// values EXPANDED inline, so only prefix/statement-level matching is
    /// sound — never count "?," tuples in the traced text.
    struct TraceCounts {
        node: AtomicUsize,
        edge: AtomicUsize,
        edge_delete: AtomicUsize,
    }
    static TRACE_COUNTS: TraceCounts = TraceCounts {
        node: AtomicUsize::new(0),
        edge: AtomicUsize::new(0),
        edge_delete: AtomicUsize::new(0),
    };
    /// The sqlite3_trace hook is connection-scoped, but this callback writes to
    /// process-global counters. Two trace-harness tests running concurrently
    /// would therefore cross-contaminate counts, so every trace-harness test
    /// holds this lock across its enable/save/disable window.
    static TRACE_HARNESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    unsafe extern "C" fn sql_trace_cb(_user: *mut c_void, sql_ptr: *const c_char) {
        if sql_ptr.is_null() {
            return;
        }
        // SAFETY: sqlite3 hands us a NUL-terminated C string for the duration
        // of the callback; we only read it, never hold it past the call.
        let sql = unsafe { CStr::from_ptr(sql_ptr) }.to_string_lossy();
        if sql.starts_with("INSERT INTO intel_nodes") {
            TRACE_COUNTS.node.fetch_add(1, Ordering::Relaxed);
        } else if sql.starts_with("INSERT INTO intel_edges") {
            TRACE_COUNTS.edge.fetch_add(1, Ordering::Relaxed);
        } else if sql.starts_with("DELETE FROM intel_edges") {
            TRACE_COUNTS.edge_delete.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Install/clear a raw `sqlite3_trace` callback on the connection.
    ///
    /// rusqlite's ergonomic `Connection::trace` is gated behind the unused
    /// `trace` crate feature; the underlying `sqlite3_trace` FFI is always
    /// available through `rusqlite::ffi`, so we call it directly to count the
    /// exact number of executed statements deterministically.
    fn set_sql_trace(conn: &rusqlite::Connection, enabled: bool) {
        let db = unsafe { conn.handle() };
        unsafe {
            if enabled {
                rusqlite::ffi::sqlite3_trace(db, Some(sql_trace_cb), std::ptr::null_mut());
            } else {
                rusqlite::ffi::sqlite3_trace(db, None, std::ptr::null_mut());
            }
        }
    }

    fn create_large_pdg(node_count: usize, edge_count: usize) -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();
        let mut node_ids = Vec::with_capacity(node_count);
        for i in 0..node_count {
            let node_id = pdg.add_node(PDGNode {
                id: format!("src/main.rs:func{i}"),
                node_type: PDGNodeType::Function,
                name: format!("func{i}"),
                file_path: Arc::from("src/main.rs"),
                byte_range: (i * 10, i * 10 + 8),
                complexity: (i % 7) as u32,
                language: "rust".to_string(),
            });
            node_ids.push(node_id);
        }
        for i in 0..edge_count {
            let a = node_ids[i];
            let b = node_ids[(i + 1) % node_count];
            pdg.add_edge(
                a,
                b,
                PDGEdge {
                    edge_type: PDGEdgeType::Call,
                    metadata: PDGEdgeMetadata {
                        call_count: Some(1),
                        variable_name: None,
                        confidence: Some(0.7),
                        channel: None,
                        position: None,
                    },
                },
            );
        }
        pdg
    }

    fn create_test_pdg() -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();

        let n1 = pdg.add_node(PDGNode {
            id: "func1".to_string(),
            node_type: PDGNodeType::Function,
            name: "func1".to_string(),
            file_path: Arc::from("test.rs"),
            byte_range: (0, 100),
            complexity: 5,
            language: "rust".to_string(),
        });

        let n2 = pdg.add_node(PDGNode {
            id: "func2".to_string(),
            node_type: PDGNodeType::Function,
            name: "func2".to_string(),
            file_path: Arc::from("test.rs"),
            byte_range: (100, 200),
            complexity: 3,
            language: "rust".to_string(),
        });

        pdg.add_edge(
            n1,
            n2,
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(5),
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );

        pdg
    }

    #[test]
    #[cfg(feature = "precision")]
    fn test_precision_marker_round_trips_through_pdg_store() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let mut pdg = create_test_pdg();
        pdg.mark_precision_symbol("func1");

        save_pdg(&mut storage, "precision_roundtrip", &pdg).unwrap();
        let loaded = load_pdg(&storage, "precision_roundtrip").unwrap();

        assert!(loaded.is_precision_symbol("func1"));
        assert!(!loaded.is_precision_symbol("func2"));
        let precision: i32 = storage
            .conn()
            .query_row(
                "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
                params!["precision_roundtrip", "func1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(precision, 1);
    }

    #[test]
    #[cfg(feature = "precision")]
    fn test_precision_marker_change_invalidates_node_noop() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let mut pdg = create_test_pdg();
        save_pdg(&mut storage, "precision_change", &pdg).unwrap();

        pdg.mark_precision_symbol("func1");
        save_pdg(&mut storage, "precision_change", &pdg).unwrap();

        let precision: i32 = storage
            .conn()
            .query_row(
                "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
                params!["precision_change", "func1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(precision, 1);

        pdg.precision_symbols.clear();
        save_pdg(&mut storage, "precision_change", &pdg).unwrap();
        let cleared: i32 = storage
            .conn()
            .query_row(
                "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
                params!["precision_change", "func1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cleared, 0);
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
        assert_eq!(node1.complexity, 5);
    }

    #[test]
    fn test_is_transient_lock_error_classification() {
        use rusqlite::ErrorCode;
        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            None,
        );
        assert!(is_transient_lock_error(&busy));
        let locked = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::DatabaseLocked,
                extended_code: 6,
            },
            None,
        );
        assert!(is_transient_lock_error(&locked));
        let constraint = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: ErrorCode::ConstraintViolation,
                extended_code: 19,
            },
            None,
        );
        assert!(!is_transient_lock_error(&constraint));
    }

    #[test]
    fn test_save_pdg_survives_competing_writer_lock() {
        // A competing connection holds the write lock briefly while save_pdg
        // runs. The busy_timeout (re-asserted by save_pdg) must wait out the
        // competing writer instead of failing the index persist.
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let pdg = create_test_pdg();

        let db_path = temp_file.path().to_path_buf();
        let holder = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(db_path).unwrap();
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS lock_probe(value INTEGER);
                 INSERT INTO lock_probe VALUES (1);",
            )
            .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(400));
            conn.execute_batch("ROLLBACK;").unwrap();
        });

        // Give the holder time to acquire the write lock before saving.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let started = std::time::Instant::now();
        save_pdg(&mut storage, "lock_proj", &pdg)
            .expect("save_pdg must wait out a brief competing write lock");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(4),
            "save_pdg should not exceed the busy window for a short lock"
        );
        holder.join().unwrap();

        let loaded = load_pdg(&storage, "lock_proj").unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);
    }

    #[test]
    fn test_large_save_pdg_survives_pinned_reader_and_wal_stays_bounded() {
        // Reproduces the stress-test report's actual failure signature: the
        // MCP server's long-lived catalog/search connections hold a read
        // snapshot that pins the WAL, so `wal_checkpoint(TRUNCATE)` cannot
        // shrink it and it grows without bound (57MB in the report). This
        // must not fail the writer — WAL readers do not block writers — and
        // once the reader releases, the WAL must be recoverable to a bounded
        // size rather than leaking forever.
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_path_buf();
        let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.display()));

        let mut storage = Storage::open(&db_path).unwrap();
        // A multi-thousand-node PDG so the write genuinely produces a
        // multi-page WAL instead of a trivial one.
        let pdg = create_large_pdg(2000, 1999);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let reader_db = db_path.clone();
        let reader = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(reader_db).unwrap();
            // Open a read transaction and hold its snapshot open: this is what
            // pins the WAL and blocks `wal_checkpoint(TRUNCATE)`.
            conn.execute_batch("BEGIN").unwrap();
            let _count: i64 = conn
                .query_row("SELECT COUNT(*) FROM intel_nodes", [], |row| row.get(0))
                .unwrap();
            ready_tx.send(()).unwrap();
            let _ = release_rx.recv();
            conn.execute_batch("COMMIT").unwrap();
        });

        // Wait until the reader has actually pinned its snapshot before writing.
        ready_rx.recv().unwrap();

        save_pdg(&mut storage, "pinned_reader_proj", &pdg)
            .expect("save_pdg must commit while a reader pins the WAL");

        let loaded = load_pdg(&storage, "pinned_reader_proj").unwrap();
        assert_eq!(loaded.node_count(), 2000);
        assert_eq!(loaded.edge_count(), 1999);

        // Release the reader and checkpoint; the WAL must shrink back to a
        // bounded size, proving the bloat came from the pinned reader and not
        // from a leaked/corrupted WAL.
        release_tx.send(()).unwrap();
        reader.join().unwrap();
        storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        let wal_len = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        assert!(
            wal_len < 1024 * 1024,
            "WAL should shrink below 1MiB after the reader releases; got {wal_len}"
        );
    }

    #[test]
    fn test_save_pdg_retries_after_busy_timeout_expires() {
        // Distinguishes the retry loop from the busy_timeout alone: the
        // competing writer holds the write lock *longer* than the 5s
        // busy_timeout, so the first attempt must fail with SQLITE_BUSY and
        // the bounded retry must rescue the save. A lock held briefly (as in
        // test_save_pdg_survives_competing_writer_lock) is absorbed by
        // busy_timeout and never exercises the retry path.
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        let pdg = create_test_pdg();

        let db_path = temp_file.path().to_path_buf();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(db_path).unwrap();
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS lock_probe(value INTEGER);
                 INSERT INTO lock_probe VALUES (1);",
            )
            .unwrap();
            held_tx.send(()).unwrap();
            // Hold past the 5s busy_timeout so the writer's first attempt
            // genuinely fails with SQLITE_BUSY and the retry loop must run.
            std::thread::sleep(std::time::Duration::from_millis(6000));
            conn.execute_batch("ROLLBACK;").unwrap();
        });

        // Start the writer only after the lock is held.
        held_rx.recv().unwrap();
        let started = std::time::Instant::now();
        save_pdg(&mut storage, "retry_proj", &pdg)
            .expect("save_pdg must retry after busy_timeout expires");
        holder.join().unwrap();

        // The save must have taken at least the full busy_timeout window,
        // proving the retry (not just busy_timeout) rescued the commit.
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(5000),
            "expected save_pdg to exhaust busy_timeout and retry"
        );
        let loaded = load_pdg(&storage, "retry_proj").unwrap();
        assert_eq!(loaded.node_count(), 2);
    }

    #[test]
    fn test_save_pdg_duplicate_node_id_keeps_all_edges_referencable() {
        // Reproduces the real stress-test root cause. The graph legitimately
        // holds multiple nodes that share the same `node_id` string (e.g.
        // `__external__` import nodes collapse to the same symbol in
        // different files). When those land in the same upsert chunk, a
        // conditional `DO UPDATE ... WHERE content_hash != excluded.content_hash`
        // used to suppress the second UPDATE, so `RETURNING id` returned fewer
        // rows than the chunk and the tail nodes were dropped from the
        // `node_id_map` — their edges then failed with `EdgeNodeMissing` and
        // the whole persist aborted. The upsert must return exactly one id per
        // tuple so every node (and thus every edge) resolves.
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = ProgramDependenceGraph::new();
        // Two distinct nodes with the SAME `id` string, so they collide on the
        // unique (project_id, node_id) index. They must both end up in the
        // node map (mapping to the same db row id is fine — the DB dedupes by
        // node_id, the graph keeps both for in-memory analysis).
        let duplicate_id = "shared.rs:collide";
        let a = pdg.add_node(PDGNode {
            id: duplicate_id.to_string(),
            node_type: PDGNodeType::External,
            name: "collide".to_string(),
            file_path: Arc::from("shared.rs"),
            byte_range: (0, 10),
            complexity: 0,
            language: "external".to_string(),
        });
        let b = pdg.add_node(PDGNode {
            id: duplicate_id.to_string(),
            node_type: PDGNodeType::External,
            name: "collide".to_string(),
            file_path: Arc::from("shared.rs"),
            byte_range: (0, 10),
            complexity: 0,
            language: "external".to_string(),
        });
        let importer = pdg.add_node(PDGNode {
            id: "src/main.rs:main".to_string(),
            node_type: PDGNodeType::Function,
            name: "main".to_string(),
            file_path: Arc::from("src/main.rs"),
            byte_range: (0, 50),
            complexity: 1,
            language: "rust".to_string(),
        });
        // Two Import edges whose targets are the colliding nodes. Before the
        // fix, the colliding node b fell out of the map and this save failed.
        pdg.add_edge(
            importer,
            a,
            PDGEdge {
                edge_type: PDGEdgeType::Import,
                metadata: PDGEdgeMetadata::empty(),
            },
        );
        pdg.add_edge(
            importer,
            b,
            PDGEdge {
                edge_type: PDGEdgeType::Import,
                metadata: PDGEdgeMetadata::empty(),
            },
        );

        save_pdg(&mut storage, "dup_proj", &pdg)
            .expect("duplicate node_id must not drop nodes from the map");

        let loaded = load_pdg(&storage, "dup_proj").unwrap();
        // The DB dedupes the duplicate node_id to a single row (unique
        // (project_id, node_id) index) and the two identical Import edges onto
        // the single (caller, callee, edge_type) row. The point of this test
        // is that the save SUCCEEDS with both edges resolved — before the fix
        // the colliding node fell out of the map and save_pdg failed with
        // `EdgeNodeMissing`.
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 1);
    }

    #[test]
    fn test_legacy_schema_missing_timestamp_columns_repaired() {
        // Simulate a database created before intel_nodes had created_at/
        // updated_at columns. `CREATE TABLE IF NOT EXISTS` never repairs an
        // existing table, so Storage::open must add the columns through the
        // ensure-columns path; otherwise save_pdg's INSERT fails with
        // "no such column" and every index generation dies at persist.
        let temp_file = NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(temp_file.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE intel_nodes (
                    id INTEGER PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    file_path TEXT NOT NULL,
                    node_id TEXT NOT NULL,
                    symbol_name TEXT NOT NULL,
                    qualified_name TEXT NOT NULL,
                    language TEXT NOT NULL DEFAULT 'unknown',
                    node_type TEXT NOT NULL,
                    signature TEXT,
                    complexity INTEGER,
                    embedding BLOB,
                    byte_range_start INTEGER,
                    byte_range_end INTEGER,
                    embedding_format INTEGER
                );
                CREATE TABLE intel_edges (
                    caller_id INTEGER NOT NULL,
                    callee_id INTEGER NOT NULL,
                    edge_type TEXT NOT NULL,
                    metadata TEXT,
                    FOREIGN KEY(caller_id) REFERENCES intel_nodes(id),
                    FOREIGN KEY(callee_id) REFERENCES intel_nodes(id),
                    PRIMARY KEY(caller_id, callee_id, edge_type)
                );",
            )
            .unwrap();
        }

        let mut storage = Storage::open(temp_file.path()).unwrap();
        let columns: Vec<String> = storage
            .conn()
            .prepare("PRAGMA table_info(intel_nodes)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(
            columns.iter().any(|column| column == "created_at"),
            "legacy schema must gain created_at"
        );
        assert!(
            columns.iter().any(|column| column == "updated_at"),
            "legacy schema must gain updated_at"
        );
        assert!(
            columns.iter().any(|column| column == "content_hash"),
            "legacy schema must gain content_hash"
        );

        // A full save/load round-trip must work on the repaired schema.
        let pdg = create_test_pdg();
        save_pdg(&mut storage, "legacy_proj", &pdg).unwrap();
        let loaded = load_pdg(&storage, "legacy_proj").unwrap();
        assert_eq!(loaded.node_count(), 2);
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
            file_path: Arc::from("new.rs"),
            byte_range: (0, 50),
            complexity: 1,
            language: "rust".to_string(),
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
        assert_eq!(
            convert_node_type(&PDGNodeType::Function),
            StorageNodeType::Function
        );
        assert_eq!(
            convert_node_type(&PDGNodeType::Class),
            StorageNodeType::Class
        );
        assert_eq!(
            convert_node_type(&PDGNodeType::Method),
            StorageNodeType::Method
        );
        assert_eq!(
            convert_node_type(&PDGNodeType::Variable),
            StorageNodeType::Variable
        );
        assert_eq!(
            convert_node_type(&PDGNodeType::Module),
            StorageNodeType::Module
        );

        assert_eq!(
            convert_storage_node_type(&StorageNodeType::Function),
            PDGNodeType::Function
        );
        assert_eq!(
            convert_storage_node_type(&StorageNodeType::Class),
            PDGNodeType::Class
        );
        assert_eq!(
            convert_storage_node_type(&StorageNodeType::Method),
            PDGNodeType::Method
        );
        assert_eq!(
            convert_storage_node_type(&StorageNodeType::Variable),
            PDGNodeType::Variable
        );
        assert_eq!(
            convert_storage_node_type(&StorageNodeType::Module),
            PDGNodeType::Module
        );

        // External node type round-trip
        assert_eq!(
            convert_node_type(&PDGNodeType::External),
            StorageNodeType::External
        );
        assert_eq!(
            convert_storage_node_type(&StorageNodeType::External),
            PDGNodeType::External
        );
    }

    #[test]
    fn test_convert_edge_types() {
        assert_eq!(convert_edge_type(&PDGEdgeType::Call), StorageEdgeType::Call);
        assert_eq!(
            convert_edge_type(&PDGEdgeType::DataDependency),
            StorageEdgeType::DataDependency
        );
        assert_eq!(
            convert_edge_type(&PDGEdgeType::Inheritance),
            StorageEdgeType::Inheritance
        );
        assert_eq!(
            convert_edge_type(&PDGEdgeType::Import),
            StorageEdgeType::Import
        );
        for (pdg, storage) in [
            (PDGEdgeType::Containment, StorageEdgeType::Containment),
            (
                PDGEdgeType::StateTransition,
                StorageEdgeType::StateTransition,
            ),
            (
                PDGEdgeType::CommandArgument,
                StorageEdgeType::CommandArgument,
            ),
            (PDGEdgeType::Environment, StorageEdgeType::Environment),
            (PDGEdgeType::Stdin, StorageEdgeType::Stdin),
        ] {
            assert_eq!(convert_edge_type(&pdg), storage);
            assert_eq!(convert_storage_edge_type(&storage), pdg);
        }

        assert_eq!(
            convert_storage_edge_type(&StorageEdgeType::Call),
            PDGEdgeType::Call
        );
        assert_eq!(
            convert_storage_edge_type(&StorageEdgeType::DataDependency),
            PDGEdgeType::DataDependency
        );
        assert_eq!(
            convert_storage_edge_type(&StorageEdgeType::Inheritance),
            PDGEdgeType::Inheritance
        );
        assert_eq!(
            convert_storage_edge_type(&StorageEdgeType::Import),
            PDGEdgeType::Import
        );
    }

    #[test]
    fn test_edge_metadata_conversion() {
        let pdg_meta = PDGEdgeMetadata {
            call_count: Some(42),
            variable_name: Some("x".to_string()),
            confidence: None,
            channel: Some("env".to_string()),
            position: Some(1),
        };

        let storage_meta = convert_edge_metadata(&pdg_meta);
        assert_eq!(storage_meta.call_count, Some(42));
        assert_eq!(storage_meta.variable_name, Some("x".to_string()));
        assert_eq!(storage_meta.channel.as_deref(), Some("env"));
        assert_eq!(storage_meta.position, Some(1));

        let converted_back = convert_storage_edge_metadata(&storage_meta);
        assert_eq!(converted_back.call_count, Some(42));
        assert_eq!(converted_back.channel.as_deref(), Some("env"));
        assert_eq!(converted_back.position, Some(1));
        assert_eq!(converted_back.variable_name, Some("x".to_string()));
    }

    #[test]
    fn test_save_pdg_with_inheritance_and_data_dependency_edges() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = ProgramDependenceGraph::new();

        let n1 = pdg.add_node(PDGNode {
            id: "child".to_string(),
            node_type: PDGNodeType::Class,
            name: "Child".to_string(),
            file_path: Arc::from("test.rs"),
            byte_range: (0, 50),
            complexity: 1,
            language: "rust".to_string(),
        });

        let n2 = pdg.add_node(PDGNode {
            id: "parent".to_string(),
            node_type: PDGNodeType::Class,
            name: "Parent".to_string(),
            file_path: Arc::from("test.rs"),
            byte_range: (50, 100),
            complexity: 1,
            language: "rust".to_string(),
        });

        let n3 = pdg.add_node(PDGNode {
            id: "data_user".to_string(),
            node_type: PDGNodeType::Function,
            name: "data_user".to_string(),
            file_path: Arc::from("test.rs"),
            byte_range: (100, 150),
            complexity: 1,
            language: "rust".to_string(),
        });

        pdg.add_edge(
            n1,
            n2,
            PDGEdge {
                edge_type: PDGEdgeType::Inheritance,
                metadata: PDGEdgeMetadata {
                    call_count: None,
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );

        pdg.add_edge(
            n3,
            n1,
            PDGEdge {
                edge_type: PDGEdgeType::DataDependency,
                metadata: PDGEdgeMetadata {
                    call_count: None,
                    variable_name: Some("child_instance".to_string()),
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );

        save_pdg(&mut storage, "test_project", &pdg).unwrap();

        let loaded = load_pdg(&storage, "test_project").unwrap();
        assert_eq!(loaded.node_count(), 3);
        assert_eq!(loaded.edge_count(), 2);

        // Verify edges by checking connectivity
        let child_id = loaded.find_by_symbol("child").unwrap();
        let parent_id = loaded.find_by_symbol("parent").unwrap();
        let data_user_id = loaded.find_by_symbol("data_user").unwrap();

        // Child should have Parent as neighbor (inheritance)
        let child_neighbors = loaded.neighbors(child_id);
        assert!(child_neighbors.contains(&parent_id));

        // data_user should have Child as neighbor (data dependency)
        let data_user_neighbors = loaded.neighbors(data_user_id);
        assert!(data_user_neighbors.contains(&child_id));
    }

    #[test]
    fn test_save_nodes_uses_fewer_than_100_inserts_for_10k_nodes() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_large_pdg(10_000, 10_000);

        let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
        TRACE_COUNTS.node.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
        set_sql_trace(storage.conn(), true);
        save_pdg(&mut storage, "big_project", &pdg).unwrap();
        set_sql_trace(storage.conn(), false);
        drop(_harness);

        let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
        assert!(
            node_stmts < 100,
            "save_nodes emitted {node_stmts} INSERT statements for 10K nodes (expected < 100)"
        );
        // Every row batch must map back: node_id_map -> db_id preserved correctly.
        let loaded = load_pdg(&storage, "big_project").unwrap();
        assert_eq!(loaded.node_count(), 10_000);
        // Sanity-check a specific sorted node survived the round trip.
        assert!(
            loaded.find_by_symbol("src/main.rs:func0").is_some(),
            "node 'src/main.rs:func0' should round-trip through batched inserts"
        );
    }

    #[test]
    fn test_save_edges_uses_fewer_than_100_inserts_for_10k_edges() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_large_pdg(10_000, 10_000);

        let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
        TRACE_COUNTS.node.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
        set_sql_trace(storage.conn(), true);
        save_pdg(&mut storage, "big_project", &pdg).unwrap();
        set_sql_trace(storage.conn(), false);
        drop(_harness);

        let edge_stmts = TRACE_COUNTS.edge.load(Ordering::Relaxed);
        assert!(
            edge_stmts < 100,
            "save_edges emitted {edge_stmts} INSERT statements for 10K edges (expected < 100)"
        );

        let loaded = load_pdg(&storage, "big_project").unwrap();
        assert_eq!(loaded.edge_count(), 10_000);
    }

    #[test]
    fn test_resave_unchanged_pdg_issues_no_node_writes() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_large_pdg(1_000, 1_000);
        save_pdg(&mut storage, "resave_proj", &pdg).unwrap();

        // Second save with an identical PDG: every node's content hash matches
        // its stored row, so the upsert path must issue ZERO node writes and
        // reuse the existing db ids. Edges are still fully rebuilt by design.
        let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
        TRACE_COUNTS.node.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
        set_sql_trace(storage.conn(), true);
        save_pdg(&mut storage, "resave_proj", &pdg).unwrap();
        set_sql_trace(storage.conn(), false);
        drop(_harness);

        let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
        assert_eq!(
            node_stmts, 0,
            "unchanged resave emitted {node_stmts} node INSERT/upsert statements (expected 0)"
        );

        let loaded = load_pdg(&storage, "resave_proj").unwrap();
        assert_eq!(loaded.node_count(), 1_000);
        assert_eq!(loaded.edge_count(), 1_000);
    }

    #[test]
    fn test_resave_with_changed_node_writes_only_changed_rows() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = create_large_pdg(100, 100);
        save_pdg(&mut storage, "resave_proj", &pdg).unwrap();

        // Mutate exactly one node's complexity; the other 99 are unchanged.
        let first_node = pdg.node_indices().next().expect("pdg has nodes");
        pdg.get_node_mut(first_node)
            .expect("node exists")
            .complexity += 1;

        let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
        TRACE_COUNTS.node.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
        set_sql_trace(storage.conn(), true);
        save_pdg(&mut storage, "resave_proj", &pdg).unwrap();
        set_sql_trace(storage.conn(), false);
        drop(_harness);

        let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
        assert_eq!(
            node_stmts, 1,
            "one-node change emitted {node_stmts} node INSERT/upsert statements (expected 1)"
        );

        let loaded = load_pdg(&storage, "resave_proj").unwrap();
        assert_eq!(loaded.node_count(), 100);
        assert_eq!(loaded.edge_count(), 100);
        // The changed node's complexity round-tripped.
        let changed = loaded.get_node(first_node).expect("node survived resave");
        assert!(changed.complexity > 0);
    }

    #[test]
    fn test_resave_unchanged_pdg_skips_trigram_blob_rewrite() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_test_pdg();
        save_pdg(&mut storage, "trigram_skip_proj", &pdg).unwrap();
        let first_updated_at: i64 = storage
            .conn()
            .query_row(
                "SELECT updated_at FROM trigram_index WHERE project_id = 'trigram_skip_proj'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        // An identical re-save must skip the blob rewrite: updated_at would
        // move forward on any write, so a stalled timestamp proves the skip.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        save_pdg(&mut storage, "trigram_skip_proj", &pdg).unwrap();
        let second_updated_at: i64 = storage
            .conn()
            .query_row(
                "SELECT updated_at FROM trigram_index WHERE project_id = 'trigram_skip_proj'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            first_updated_at, second_updated_at,
            "unchanged trigram index must not be rewritten"
        );
    }

    #[test]
    fn test_resave_unchanged_pdg_issues_no_edge_writes() {
        // The save-PDG hot path: an identical re-save used to DELETE and
        // re-INSERT every edge row (110K+ on large projects). The edge diff
        // must issue ZERO edge write statements when nothing changed.
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let pdg = create_large_pdg(1_000, 1_000);
        save_pdg(&mut storage, "edge_resave_proj", &pdg).unwrap();

        let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
        TRACE_COUNTS.node.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
        TRACE_COUNTS.edge_delete.store(0, Ordering::Relaxed);
        set_sql_trace(storage.conn(), true);
        save_pdg(&mut storage, "edge_resave_proj", &pdg).unwrap();
        set_sql_trace(storage.conn(), false);
        drop(_harness);

        let inserted = TRACE_COUNTS.edge.load(Ordering::Relaxed);
        let deleted = TRACE_COUNTS.edge_delete.load(Ordering::Relaxed);
        assert_eq!(
            inserted, 0,
            "unchanged resave emitted {inserted} edge INSERT statements (expected 0)"
        );
        assert_eq!(
            deleted, 0,
            "unchanged resave emitted {deleted} edge DELETE statements (expected 0)"
        );

        let loaded = load_pdg(&storage, "edge_resave_proj").unwrap();
        assert_eq!(loaded.node_count(), 1_000);
        assert_eq!(loaded.edge_count(), 1_000);
    }

    #[test]
    fn test_resave_edge_delta_persists_exactly_the_delta() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let mut pdg = create_large_pdg(100, 99);
        save_pdg(&mut storage, "edge_delta_proj", &pdg).unwrap();

        // Delta: flip one edge's call_count, drop the LAST edge, add one
        // node+edge. (Legacy sqlite3_trace expands bound values into the SQL
        // text, so row counts are asserted through persisted DB state rather
        // than statement text.)
        let first_edge = pdg.edge_indices().next().expect("pdg has edges");
        pdg.graph[first_edge].metadata.call_count = Some(42);
        let last_edge = pdg.edge_indices().last().expect("pdg has edges");
        let (last_from, last_to) = pdg.edge_endpoints(last_edge).expect("endpoints");
        let removed_pair = (
            pdg.get_node(last_from).expect("node").id.clone(),
            pdg.get_node(last_to).expect("node").id.clone(),
        );
        pdg.remove_edge(last_edge);
        let extra = pdg.add_node(PDGNode {
            id: "src/main.rs:extra".to_string(),
            node_type: PDGNodeType::Function,
            name: "extra".to_string(),
            file_path: Arc::from("src/main.rs"),
            byte_range: (9_000, 9_010),
            complexity: 1,
            language: "rust".to_string(),
        });
        let any_node = pdg.node_indices().next().expect("pdg has nodes");
        pdg.add_edge(
            any_node,
            extra,
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(7),
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );

        save_pdg(&mut storage, "edge_delta_proj", &pdg).unwrap();

        // Persisted state reflects exactly the delta:
        let loaded = load_pdg(&storage, "edge_delta_proj").unwrap();
        assert_eq!(loaded.node_count(), 101);
        // 99 - 1 removed + 1 added.
        assert_eq!(loaded.edge_count(), 99);
        // The changed edge's metadata round-tripped.
        let changed = loaded
            .edge_indices()
            .map(|edge| loaded.get_edge(edge).expect("edge"))
            .filter(|edge| edge.metadata.call_count == Some(42))
            .count();
        assert_eq!(changed, 1, "changed edge metadata must persist");
        // The new edge exists with its distinctive call_count.
        let added = loaded
            .edge_indices()
            .map(|edge| loaded.get_edge(edge).expect("edge"))
            .filter(|edge| edge.metadata.call_count == Some(7))
            .count();
        assert_eq!(added, 1, "new edge must persist");
        // The removed edge is gone: no edge connects the removed pair.
        let (from_id, to_id) = &removed_pair;
        let from_loaded = loaded.find_by_id(from_id).expect("from node survived");
        let to_loaded = loaded.find_by_id(to_id).expect("to node survived");
        let still_connected = loaded.edge_indices().any(|edge| {
            let (source, target) = loaded.edge_endpoints(edge).expect("endpoints");
            (source == from_loaded && target == to_loaded)
                || (source == to_loaded && target == from_loaded)
        });
        assert!(!still_connected, "removed edge must not survive the resave");
    }

    #[test]
    fn test_resave_after_node_removal_cleans_its_edges() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        // Path graph 0→1→...→49: node 0 participates in exactly ONE edge.
        let mut pdg = create_large_pdg(50, 49);
        save_pdg(&mut storage, "node_removal_proj", &pdg).unwrap();

        let victim = pdg.node_indices().next().expect("pdg has nodes");
        let victim_id = pdg.get_node(victim).expect("node").id.clone();
        pdg.remove_node(victim);

        save_pdg(&mut storage, "node_removal_proj", &pdg).unwrap();

        let loaded = load_pdg(&storage, "node_removal_proj").unwrap();
        assert_eq!(loaded.node_count(), 49);
        assert_eq!(
            loaded.edge_count(),
            48,
            "node 0's single edge must be removed with it (49 - 1)"
        );
        assert!(loaded.find_by_id(&victim_id).is_none());
    }

    #[test]
    fn test_save_pdg_bulk_rebuild_on_major_edge_churn() {
        // When most edges churn (different node identities), the diff takes
        // the bulk subquery DELETE + full reinsert path. Correctness must
        // match the row-by-row path.
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        let before = create_large_pdg(2_000, 1_999);
        save_pdg(&mut storage, "churn_proj", &before).unwrap();

        // Completely different node ids (different file path) ⇒ zero shared
        // edges ⇒ stale fraction 100% ⇒ bulk path.
        let after = {
            let mut pdg = ProgramDependenceGraph::new();
            let mut ids = Vec::new();
            for i in 0..500 {
                ids.push(pdg.add_node(PDGNode {
                    id: format!("src/other.rs:func{i}"),
                    node_type: PDGNodeType::Function,
                    name: format!("func{i}"),
                    file_path: Arc::from("src/other.rs"),
                    byte_range: (i * 10, i * 10 + 8),
                    complexity: (i % 5) as u32,
                    language: "rust".to_string(),
                }));
            }
            for i in 0..499 {
                pdg.add_edge(
                    ids[i],
                    ids[i + 1],
                    PDGEdge {
                        edge_type: PDGEdgeType::Call,
                        metadata: PDGEdgeMetadata {
                            call_count: Some(1),
                            variable_name: None,
                            confidence: None,
                            channel: None,
                            position: None,
                        },
                    },
                );
            }
            pdg
        };
        save_pdg(&mut storage, "churn_proj", &after).unwrap();

        let loaded = load_pdg(&storage, "churn_proj").unwrap();
        assert_eq!(
            loaded.node_count(),
            500,
            "old nodes must be pruned on churn"
        );
        assert_eq!(loaded.edge_count(), 499);
        assert!(
            loaded.find_by_symbol("src/other.rs:func0").is_some(),
            "new graph must fully replace the old one"
        );
    }

    #[test]
    fn test_batch_delete_equivalent_to_single() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        // Build a PDG with nodes across 5 files
        let mut pdg = ProgramDependenceGraph::new();
        let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
        let mut node_ids = Vec::new();
        for (fi, file) in files.iter().enumerate() {
            for ni in 0..3 {
                let id = pdg.add_node(PDGNode {
                    id: format!("{file}:func{ni}"),
                    node_type: PDGNodeType::Function,
                    name: format!("func_{fi}_{ni}"),
                    file_path: Arc::from(*file),
                    byte_range: (ni * 10, ni * 10 + 5),
                    complexity: ni as u32,
                    language: "rust".to_string(),
                });
                node_ids.push(id);
            }
        }
        // Add some edges between nodes in different files
        for i in 0..(node_ids.len() - 1) {
            pdg.add_edge(
                node_ids[i],
                node_ids[i + 1],
                PDGEdge {
                    edge_type: PDGEdgeType::Call,
                    metadata: PDGEdgeMetadata {
                        call_count: Some(1),
                        variable_name: None,
                        confidence: None,
                        channel: None,
                        position: None,
                    },
                },
            );
        }
        save_pdg(&mut storage, "proj_batch_del", &pdg).unwrap();

        // Delete 3 files via single calls (a.rs, b.rs, c.rs)
        delete_file_data(&mut storage, "proj_batch_del", "a.rs").unwrap();
        delete_file_data(&mut storage, "proj_batch_del", "b.rs").unwrap();
        delete_file_data(&mut storage, "proj_batch_del", "c.rs").unwrap();

        let loaded_single = load_pdg(&storage, "proj_batch_del").unwrap();
        let single_nodes = loaded_single.node_count();
        let single_edges = loaded_single.edge_count();

        // Now test batch delete on a fresh copy
        let temp_file2 = NamedTempFile::new().unwrap();
        let mut storage2 = Storage::open(temp_file2.path()).unwrap();
        save_pdg(&mut storage2, "proj_batch_del", &pdg).unwrap();

        let tx = storage2.conn_mut().transaction().unwrap();
        delete_files_data_tx(
            &tx,
            "proj_batch_del",
            &["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
        )
        .unwrap();
        tx.commit().unwrap();

        let loaded_batch = load_pdg(&storage2, "proj_batch_del").unwrap();
        assert_eq!(loaded_batch.node_count(), single_nodes);
        assert_eq!(loaded_batch.edge_count(), single_edges);
        // 5 files * 3 nodes each = 15 total, deleted 3 files * 3 = 9, expect 6 remaining
        assert_eq!(single_nodes, 6);
    }

    #[test]
    fn test_batch_update_equivalent_to_single() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        // Update 10 indexed_files via single calls
        for i in 0..10 {
            update_indexed_file(
                &mut storage,
                "proj_batch_upd",
                &format!("src/file{i}.rs"),
                &format!("hash{i}"),
            )
            .unwrap();
        }
        let single_files = get_indexed_files(&storage, "proj_batch_upd").unwrap();

        // Batch update on a fresh storage
        let temp_file2 = NamedTempFile::new().unwrap();
        let mut storage2 = Storage::open(temp_file2.path()).unwrap();

        let files: Vec<(String, String)> = (0..10)
            .map(|i| (format!("src/file{i}.rs"), format!("hash{i}")))
            .collect();
        let tx = storage2.conn_mut().transaction().unwrap();
        update_indexed_files_tx(&tx, "proj_batch_upd", &files).unwrap();
        tx.commit().unwrap();

        let batch_files = get_indexed_files(&storage2, "proj_batch_upd").unwrap();

        assert_eq!(batch_files.len(), single_files.len());
        for (path, hash) in &single_files {
            assert_eq!(batch_files.get(path), Some(hash));
        }
    }

    #[test]
    fn test_pdg_roundtrip_preserves_data() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        // Create PDG with 50 nodes, 49 edges across 10 files
        let mut pdg = ProgramDependenceGraph::new();
        let files: Vec<String> = (0..10).map(|i| format!("src/mod{i}.rs")).collect();
        let mut node_ids = Vec::new();
        for i in 0..50 {
            let file = &files[i % 10];
            let id = pdg.add_node(PDGNode {
                id: format!("{file}:func{i}"),
                node_type: PDGNodeType::Function,
                name: format!("func{i}"),
                file_path: Arc::from(file.as_str()),
                byte_range: (i * 20, i * 20 + 10),
                complexity: (i % 5) as u32,
                language: "rust".to_string(),
            });
            node_ids.push(id);
        }
        for i in 0..49 {
            pdg.add_edge(
                node_ids[i],
                node_ids[i + 1],
                PDGEdge {
                    edge_type: PDGEdgeType::Call,
                    metadata: PDGEdgeMetadata {
                        call_count: Some(i + 1),
                        variable_name: None,
                        confidence: Some(0.9),
                        channel: None,
                        position: None,
                    },
                },
            );
        }

        save_pdg(&mut storage, "proj_roundtrip", &pdg).unwrap();
        let loaded = load_pdg(&storage, "proj_roundtrip").unwrap();

        assert_eq!(loaded.node_count(), 50);
        assert_eq!(loaded.edge_count(), 49);

        // Verify a sampling of nodes survived intact
        for i in [0, 10, 25, 49] {
            let file = &files[i % 10];
            let sym = format!("{file}:func{i}");
            assert!(loaded.find_by_symbol(&sym).is_some(), "node {sym} missing");
        }
    }

    #[test]
    fn test_incremental_refresh_no_corruption() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();

        // Phase 1: Add file_a with 2 nodes
        let mut pdg_a = ProgramDependenceGraph::new();
        let a1 = pdg_a.add_node(PDGNode {
            id: "a.rs:alpha".to_string(),
            node_type: PDGNodeType::Function,
            name: "alpha".to_string(),
            file_path: Arc::from("a.rs"),
            byte_range: (0, 50),
            complexity: 1,
            language: "rust".to_string(),
        });
        let a2 = pdg_a.add_node(PDGNode {
            id: "a.rs:beta".to_string(),
            node_type: PDGNodeType::Function,
            name: "beta".to_string(),
            file_path: Arc::from("a.rs"),
            byte_range: (50, 100),
            complexity: 1,
            language: "rust".to_string(),
        });
        pdg_a.add_edge(
            a1,
            a2,
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(1),
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );
        save_pdg(&mut storage, "proj_refresh", &pdg_a).unwrap();
        update_indexed_file(&mut storage, "proj_refresh", "a.rs", "hash_a_v1").unwrap();

        // Phase 2: Delete a.rs, then re-add with modified content
        delete_file_data(&mut storage, "proj_refresh", "a.rs").unwrap();

        let mut pdg_a2 = ProgramDependenceGraph::new();
        pdg_a2.add_node(PDGNode {
            id: "a.rs:alpha".to_string(),
            node_type: PDGNodeType::Function,
            name: "alpha".to_string(),
            file_path: Arc::from("a.rs"),
            byte_range: (0, 60),
            complexity: 2,
            language: "rust".to_string(),
        });
        pdg_a2.add_node(PDGNode {
            id: "a.rs:gamma".to_string(),
            node_type: PDGNodeType::Function,
            name: "gamma".to_string(),
            file_path: Arc::from("a.rs"),
            byte_range: (60, 120),
            complexity: 3,
            language: "rust".to_string(),
        });

        // Use transaction-aware delete + save for incremental update
        let tx = storage.conn_mut().transaction().unwrap();
        delete_file_data_tx(&tx, "proj_refresh", "a.rs").unwrap();
        tx.commit().unwrap();

        save_pdg(&mut storage, "proj_refresh", &pdg_a2).unwrap();
        update_indexed_file(&mut storage, "proj_refresh", "a.rs", "hash_a_v2").unwrap();

        // Verify: no orphaned nodes/edges, correct data
        let loaded = load_pdg(&storage, "proj_refresh").unwrap();
        assert_eq!(loaded.node_count(), 2);
        assert_eq!(loaded.edge_count(), 0); // pdg_a2 has no edges

        // Verify the right nodes are present
        assert!(loaded.find_by_symbol("a.rs:alpha").is_some());
        assert!(loaded.find_by_symbol("a.rs:gamma").is_some());
        assert!(loaded.find_by_symbol("a.rs:beta").is_none()); // old node should be gone

        // Verify indexed_files is correct
        let indexed = get_indexed_files(&storage, "proj_refresh").unwrap();
        assert_eq!(indexed.get("a.rs"), Some(&"hash_a_v2".to_string()));
    }
}
