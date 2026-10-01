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
    let (node_id_map, stale_node_ids, project_rows) = save_nodes(&tx, project_id, pdg)?;
    let nodes_elapsed = started.elapsed();

    let edge_stats = save_edges(&tx, project_id, &project_rows, &node_id_map, pdg)?;
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

/// Persisted state of one `intel_nodes` row: `(db_id, content_hash, precision)`.
type ExistingNodeRow = (i64, String, bool);

/// Pre-query existing rows for the project so an unchanged node reuses its db
/// id and issues no write (C1). A legacy row whose content_hash was computed
/// as blake3(node_id) under the old scheme reads as changed once and is
/// rewritten under the new scheme on the first save.
fn load_existing_node_rows(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
) -> Result<HashMap<String, ExistingNodeRow>> {
    let mut existing: HashMap<String, ExistingNodeRow> = HashMap::new();
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
    Ok(existing)
}

/// Build the `NodeRecord` to persist for one PDG node, including the freshly
/// computed content hash (C3: one hash per node, reused for the column and
/// the skip check).
fn changed_node_record(
    pdg_node: &PDGNode,
    project_id: &str,
    qualified_name: String,
    content_hash: String,
    graph_precision: bool,
) -> NodeRecord {
    // Note: Embeddings are externalized to EmbeddingStore, not stored here.
    NodeRecord {
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
    }
}

/// Collect the db ids of every row currently persisted for the project.
fn persisted_node_row_ids(existing: &HashMap<String, ExistingNodeRow>) -> RowIdSet {
    existing.values().map(|(db_id, _, _)| *db_id).collect()
}

/// Diff the graph's nodes against the persisted rows (C1/C3): unchanged nodes
/// map straight to their db id, new/changed ones are queued for upsert. Also
/// returns the stale ids (rows in the DB absent from the new PDG).
fn diff_nodes_against_persisted_rows(
    pdg: &ProgramDependenceGraph,
    project_id: &str,
    existing: &HashMap<String, ExistingNodeRow>,
) -> Result<(NodeRowMap, Vec<(usize, NodeRecord)>, Vec<String>)> {
    let node_indices: Vec<NodeId> = pdg.node_indices().collect();
    let mut node_id_map =
        NodeRowMap::with_capacity_and_hasher(node_indices.len(), Default::default());
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

        let record = changed_node_record(
            pdg_node,
            project_id,
            qualified_name,
            content_hash,
            graph_precision,
        );
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

    Ok((node_id_map, to_upsert, stale_node_ids))
}

fn save_nodes(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<(NodeRowMap, Vec<String>, RowIdSet)> {
    let existing = load_existing_node_rows(tx, project_id)?;
    let (mut node_id_map, to_upsert, stale_node_ids) =
        diff_nodes_against_persisted_rows(pdg, project_id, &existing)?;
    upsert_changed_nodes(tx, &node_indices(pdg), &mut node_id_map, &to_upsert)?;
    let project_rows = persisted_node_row_ids(&existing);
    Ok((node_id_map, stale_node_ids, project_rows))
}

/// Node indices in the same order `diff_nodes_against_persisted_rows`
/// collected them, so `(pos, _)` chunk pairs resolve back to graph nodes.
fn node_indices(pdg: &ProgramDependenceGraph) -> Vec<NodeId> {
    pdg.node_indices().collect()
}

/// SQL text for one batched node upsert: `values_clause` rows of 17 anonymous
/// placeholders, each row aligned positionally with [`node_record_sql_params`].
fn node_upsert_sql(values_clause: &str) -> String {
    format!(
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
    )
}

/// Bind one node record's upsert parameters, positionally aligned with the
/// statement built by [`node_upsert_sql`].
fn node_record_sql_params(record: &NodeRecord, now: i64) -> Vec<Value> {
    vec![
        record.project_id.clone().into(),
        record.file_path.clone().into(),
        record.node_id.clone().into(),
        record.symbol_name.clone().into(),
        record.qualified_name.clone().into(),
        record.language.clone().into(),
        record.node_type.as_str().to_string().into(),
        record
            .signature
            .clone()
            .map(Value::Text)
            .unwrap_or(Value::Null),
        record
            .complexity
            .map(|c| Value::Integer(c.into()))
            .unwrap_or(Value::Null),
        record.content_hash.clone().into(),
        record
            .embedding
            .clone()
            .map(Value::Blob)
            .unwrap_or(Value::Null),
        record
            .byte_range_start
            .map(Value::Integer)
            .unwrap_or(Value::Null),
        record
            .byte_range_end
            .map(Value::Integer)
            .unwrap_or(Value::Null),
        Value::Integer(now),
        Value::Integer(now),
        record
            .embedding_format
            .map(|f| Value::Integer(f.into()))
            .unwrap_or(Value::Null),
        Value::Integer(record.precision as i64),
    ]
}

/// Whether `RETURNING id` produced exactly one db id per VALUES tuple. The
/// statement must return EXACTLY one row per tuple; if that ever regresses
/// (e.g. a future conditional guard), fail loudly rather than silently
/// dropping nodes from the map and corrupting edge references.
fn returned_row_count_mismatch(ids: &[i64], tuple_count: usize) -> bool {
    ids.len() != tuple_count
}

/// Upsert only new/changed nodes in batches. `RETURNING id` emits rows in
/// the same order as the VALUES tuples, so the returned db ids map 1:1 onto
/// the chunk's (pos, record) pairs. The invariant that makes this mapping
/// sound is that the statement returns EXACTLY one row per input tuple.
///
/// A previous `DO UPDATE ... WHERE content_hash != excluded.content_hash`
/// guard violated that invariant: when a chunk contains duplicate
/// `node_id`s (the graph legitimately holds same-named symbols from
/// different files), the second tuple's guard evaluated false, suppressed
/// the UPDATE, and `RETURNING` returned fewer rows than the chunk. The
/// `zip` below then silently dropped the tail nodes from `node_id_map`,
/// so their edges referenced non-existent rows and the whole persist
/// failed with `EdgeNodeMissing`. Unchanged rows are already filtered out
/// by the Rust-side content_hash comparison, so the guard was both
/// redundant and harmful; it is removed here.
fn upsert_changed_nodes(
    tx: &rusqlite::Transaction<'_>,
    node_indices: &[NodeId],
    node_id_map: &mut NodeRowMap,
    to_upsert: &[(usize, NodeRecord)],
) -> Result<()> {
    for chunk in to_upsert.chunks(PDG_INSERT_BATCH_SIZE) {
        let n = chunk.len();

        // Each row carries its own set of 17 anonymous `?` placeholders, bound
        // positionally so they align with `params` below.
        let values_clause = (0..n)
            .map(|_| "(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .collect::<Vec<_>>()
            .join(", ");
        let sql = node_upsert_sql(&values_clause);

        let now = chrono::Utc::now().timestamp();
        let mut params: Vec<rusqlite::types::Value> = Vec::with_capacity(n * 17);
        for (_, record) in chunk {
            params.extend(node_record_sql_params(record, now));
        }

        let mut stmt = tx.prepare(&sql)?;
        let ids: Vec<i64> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if returned_row_count_mismatch(&ids, chunk.len()) {
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
    Ok(())
}

/// Persisted-row identity of an edge: the `intel_edges` primary key.
type EdgeKey = (i64, i64, &'static str);

/// Metadata JSON of an edge as compared during the diff: `None` is the
/// all-null literal that ~70% of edges carry, so most keys hold no string at all.
type EdgeMeta = Option<String>;

type EdgeMap =
    HashMap<EdgeKey, EdgeMeta, std::hash::BuildHasherDefault<crate::fast_hash::FastHasher>>;

/// Every persisted edge row for the project (caller-side membership checked
/// in memory), keyed like the old bulk DELETE: `(caller, callee, type)`.
/// One sequential read (no WAL growth) instead of a full-table rewrite. NULL
/// metadata (legacy rows) reads as the empty string, which never equals
/// serialized JSON, so such rows are rewritten once and converge.
///
/// A sequential scan of the table with an in-memory membership test is ~4x
/// faster than the equivalent `caller_id IN (SELECT ...)` semi-join, which
/// probed the primary-key index once per node.
fn load_existing_edge_rows(
    tx: &rusqlite::Transaction<'_>,
    project_rows: &RowIdSet,
) -> Result<(EdgeMap, Vec<(i64, i64, String)>)> {
    let mut existing = EdgeMap::default();
    // Rows whose type this build does not know: never desired, so always stale.
    let mut unknown_type: Vec<(i64, i64, String)> = Vec::new();
    let mut stmt =
        tx.prepare("SELECT caller_id, callee_id, edge_type, metadata FROM intel_edges")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let caller: i64 = row.get(0)?;
        if !project_rows.contains(&caller) {
            continue;
        }
        let callee: i64 = row.get(1)?;
        let edge_type = row
            .get_ref(2)?
            .as_str()
            .map_err(|e| PdgStoreError::Deserialization(e.to_string()))?;
        let metadata: EdgeMeta = match row.get_ref(3)? {
            rusqlite::types::ValueRef::Text(bytes) if bytes == EMPTY_EDGE_METADATA_JSON => None,
            rusqlite::types::ValueRef::Text(bytes) => {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
            // NULL (legacy rows) reads as a value that never equals real
            // JSON, so such rows are rewritten once and converge.
            _ => Some(String::new()),
        };
        match StorageEdgeType::from_str_name(edge_type) {
            Some(kind) => {
                existing.insert((caller, callee, kind.as_str()), metadata);
            }
            None => unknown_type.push((caller, callee, edge_type.to_string())),
        }
    }
    Ok((existing, unknown_type))
}

/// Serialize an edge's metadata the way it is stored and diffed: an edge with
/// every field `None` stores `None` (recognized on load without parsing),
/// everything else its JSON form.
fn edge_metadata_json(metadata: &StorageEdgeMetadata) -> Result<EdgeMeta> {
    if metadata.call_count.is_none()
        && metadata.variable_name.is_none()
        && metadata.confidence.is_none()
        && metadata.channel.is_none()
        && metadata.position.is_none()
    {
        Ok(None)
    } else {
        Ok(Some(serde_json::to_string(metadata).map_err(|e| {
            PdgStoreError::Serialization(e.to_string())
        })?))
    }
}

/// Row id of one edge endpoint; a missing endpoint is an error reporting the
/// edge's (caller, callee) graph indices, whichever end is missing.
fn edge_endpoint_row_id(
    node_id_map: &NodeRowMap,
    source: &NodeId,
    target: &NodeId,
    endpoint: &NodeId,
) -> Result<i64> {
    node_id_map
        .get(endpoint)
        .copied()
        .ok_or_else(|| PdgStoreError::EdgeNodeMissing {
            caller: source.index() as i64,
            callee: target.index() as i64,
        })
}

/// Every edge of the graph, decoded into the desired persisted-row map. Rows
/// whose caller or callee has no persisted row are an error (the node save
/// guarantees every graph node one).
fn desired_edge_map(pdg: &ProgramDependenceGraph, node_id_map: &NodeRowMap) -> Result<EdgeMap> {
    let mut desired = EdgeMap::with_capacity_and_hasher(pdg.edge_count(), Default::default());
    for edge_idx in pdg.edge_indices() {
        let (source, target) = pdg
            .edge_endpoints(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Edge has no endpoints".to_string()))?;
        let pdg_edge = pdg
            .get_edge(edge_idx)
            .ok_or_else(|| PdgStoreError::Serialization("Missing edge data".to_string()))?;
        let caller_id = edge_endpoint_row_id(node_id_map, &source, &target, &source)?;
        let callee_id = edge_endpoint_row_id(node_id_map, &source, &target, &target)?;
        let metadata = convert_edge_metadata(&pdg_edge.metadata);
        desired.insert(
            (
                caller_id,
                callee_id,
                convert_edge_type(&pdg_edge.edge_type).as_str(),
            ),
            edge_metadata_json(&metadata)?,
        );
    }
    Ok(desired)
}

/// Whether the edge diff churns enough of the table to switch to the bulk
/// path (major refactor / different content): the single subquery DELETE then
/// beats thousands of parameterized deletes and every desired edge becomes a
/// fresh insert — i.e. the old full-rebuild path.
fn is_bulk_delete(existing_total: usize, stale_len: usize) -> bool {
    existing_total >= 64 && stale_len * 2 > existing_total
}

/// Delete exactly the stale rows: the bulk subquery DELETE when `bulk`, else
/// row-value deletes over a VALUES table keyed by the (caller_id, callee_id,
/// edge_type) primary key.
///
/// The row-value IN is flat (no expression-tree depth growth — a chain of
/// ORs nests left-associatively and blows SQLITE_LIMIT_EXPR_DEPTH=1000 at
/// ~1000 rows), and the planner can use the (caller_id, callee_id, edge_type)
/// PK index for the probe. 3 bound params per row; 1000 rows = 3000 params,
/// well under SQLITE_MAX_VARIABLE_NUMBER (32766).
fn delete_stale_edges(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    stale: &[(i64, i64, String)],
    bulk: bool,
    stats: &mut EdgeSaveStats,
) -> Result<()> {
    if stale.is_empty() {
        // Nothing to delete.
        return Ok(());
    }
    if bulk {
        tx.execute(
            "DELETE FROM intel_edges WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)",
            params![project_id],
        )?;
        stats.deleted = stale.len();
        return Ok(());
    }
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
    Ok(())
}

/// Rows to write after the delete phase: after a bulk delete every desired
/// edge is a fresh insert; otherwise only new rows and rows whose metadata
/// JSON changed (the upsert's DO UPDATE arm covers changed metadata).
fn plan_edge_writes<'a>(
    desired: &'a EdgeMap,
    existing: &EdgeMap,
    bulk: bool,
) -> Vec<(i64, i64, &'static str, &'a EdgeMeta)> {
    let mut to_write: Vec<(i64, i64, &'static str, &EdgeMeta)> = Vec::new();
    for (key, metadata) in desired {
        let unchanged = !bulk && existing.get(key).is_some_and(|stored| stored == metadata);
        if !unchanged {
            to_write.push((key.0, key.1, key.2, metadata));
        }
    }
    to_write
}

/// Write the planned rows in batched multi-row upserts, mirroring the node
/// batching.
fn write_edge_rows(
    tx: &rusqlite::Transaction<'_>,
    to_write: &[(i64, i64, &'static str, &EdgeMeta)],
    stats: &mut EdgeSaveStats,
) -> Result<()> {
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
            params.push(Value::Text((*edge_type).to_string()));
            params.push(Value::Text(match metadata_json {
                Some(json) => json.clone(),
                None => String::from_utf8_lossy(EMPTY_EDGE_METADATA_JSON).into_owned(),
            }));
        }

        tx.execute(&sql, rusqlite::params_from_iter(params.iter()))?;
        stats.written += n;
    }
    Ok(())
}

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
    project_rows: &RowIdSet,
    node_id_map: &NodeRowMap,
    pdg: &ProgramDependenceGraph,
) -> Result<EdgeSaveStats> {
    let (existing, unknown_type) = load_existing_edge_rows(tx, project_rows)?;
    let desired = desired_edge_map(pdg, node_id_map)?;

    // Stale = persisted rows absent from the desired set (includes every edge
    // of a removed node, since those keys cannot be produced from the current
    // node_id_map).
    let mut stale: Vec<(i64, i64, String)> = existing
        .keys()
        .filter(|key| !desired.contains_key(key))
        .map(|&(caller, callee, kind)| (caller, callee, kind.to_string()))
        .collect();
    let existing_total = existing.len() + unknown_type.len();
    stale.extend(unknown_type);

    let mut stats = EdgeSaveStats::default();

    let bulk = is_bulk_delete(existing_total, stale.len());
    delete_stale_edges(tx, project_id, &stale, bulk, &mut stats)?;

    let to_write = plan_edge_writes(&desired, &existing, bulk);
    write_edge_rows(tx, &to_write, &mut stats)?;

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

/// Graph node -> row id, hashed the same way.
type NodeRowMap = HashMap<NodeId, i64, std::hash::BuildHasherDefault<crate::fast_hash::FastHasher>>;

type RowIdSet =
    std::collections::HashSet<i64, std::hash::BuildHasherDefault<crate::fast_hash::FastHasher>>;

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
        let (pdg_node, precision) = decode_pdg_node(row, node_type, file_path)?;
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
#[path = "pdg_store_test.rs"]
mod tests;
