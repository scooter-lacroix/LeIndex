//! Bounded, read-only point lookups over persisted index nodes.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::graph::pdg::{Node, ProgramDependenceGraph};

const MAX_CATALOG_ROWS: usize = 200;
const MAX_POOLED_CATALOG_CONNECTIONS: usize = 16;

static CATALOG_CONNECTIONS: std::sync::OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Connection>>>>> =
    std::sync::OnceLock::new();

/// A symbol record needed by MCP exact-read responses.
#[derive(Debug, Clone)]
pub struct CatalogSymbol {
    /// Persisted node identifier.
    pub node_id: String,
    /// Unqualified symbol name.
    pub symbol_name: String,
    /// Qualified symbol name.
    pub qualified_name: String,
    /// Canonical source path.
    pub file_path: PathBuf,
    /// Parser language.
    pub language: String,
    /// Persisted node kind.
    pub node_type: String,
    /// Cyclomatic complexity, when recorded.
    pub complexity: u32,
    /// Byte offsets in the source file.
    pub byte_range: (usize, usize),
}

/// Build the catalog view of one graph node. `qualified_name` is derived the
/// same way the SQL writer derived it (`node.id.split(':').next_back()` —
/// see `pdg_store::diff_nodes_against_persisted_rows`), so graph-backed and
/// SQL-backed reads report identical values.
fn catalog_symbol_from_node(node: &Node) -> CatalogSymbol {
    let qualified_name = node.id.rsplit(':').next().unwrap_or(&node.id).to_string();
    CatalogSymbol {
        node_id: node.id.clone(),
        symbol_name: node.name.clone(),
        qualified_name,
        file_path: PathBuf::from(node.file_path.to_string()),
        language: node.language.clone(),
        node_type: crate::storage::generation::graph_codec::graph_node_type_str(&node.node_type)
            .to_string(),
        complexity: node.complexity,
        byte_range: node.byte_range,
    }
}

/// Normalized root-relative form of `file` (absolute, root-relative, or
/// `..`-laden — handlers resolve live paths while stored nodes and legacy
/// layers may use either form).
fn relative_file(root: &Path, file: &Path) -> String {
    let joined = if file.is_absolute() {
        file.to_path_buf()
    } else {
        root.join(file)
    };
    let stripped = joined.strip_prefix(root).unwrap_or(&joined);
    let mut rel = PathBuf::new();
    for component in stripped.components() {
        match component {
            std::path::Component::ParentDir => {
                rel.pop();
            }
            std::path::Component::CurDir => {}
            other => rel.push(other.as_os_str()),
        }
    }
    rel.to_string_lossy().replace('\\', "/")
}

/// Whether `node_path` refers to the filter file. Graph nodes may store
/// absolute OR root-relative paths (production graphs mix both — see
/// `textindex::symbols_from_pdg`), so compare normalized root-relative forms.
fn file_matches(root: &Path, node_path: &str, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    let node = Path::new(node_path);
    let node_rel = node
        .strip_prefix(root)
        .unwrap_or(node)
        .to_string_lossy()
        .replace('\\', "/");
    node_rel == filter
}

/// D6 graph-backed catalog queries: the resident PDG or the published Pdg
/// layer is the graph store; ordering/cap semantics mirror the SQL
/// statements they replaced (see each function's doc).
pub mod graph {
    use super::*;

    /// Exact-then-case-insensitive symbol lookup over the resident graph.
    pub fn find_symbol(
        pdg: &ProgramDependenceGraph,
        root: &Path,
        symbol: &str,
        file: Option<&Path>,
    ) -> Vec<CatalogSymbol> {
        let file_str = file.map(|file| relative_file(root, file));
        let mut matches: Vec<CatalogSymbol> = pdg
            .node_indices()
            .filter_map(|index| pdg.get_node(index))
            .filter(|node| file_matches(root, &node.file_path, file_str.as_deref()))
            .filter(|node| {
                let qualified = node.id.rsplit(':').next().unwrap_or(&node.id);
                node.name == symbol
                    || qualified == symbol
                    || node.name.eq_ignore_ascii_case(symbol)
                    || qualified.eq_ignore_ascii_case(symbol)
            })
            .map(catalog_symbol_from_node)
            .collect();
        // SQL: ORDER BY CASE WHEN symbol_name = ? OR qualified_name = ? THEN 0
        // ELSE 1 END, node_id.
        matches.sort_by(|a, b| {
            let rank = |candidate: &CatalogSymbol| {
                if candidate.symbol_name == symbol || candidate.qualified_name == symbol {
                    0u8
                } else {
                    1u8
                }
            };
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        matches.truncate(MAX_CATALOG_ROWS);
        matches
    }

    /// Bounded symbol inventory for one file, ordered by byte range then id.
    pub fn symbols_in_file(
        pdg: &ProgramDependenceGraph,
        root: &Path,
        file: &Path,
    ) -> Vec<CatalogSymbol> {
        let file_str = relative_file(root, file);
        let mut symbols: Vec<CatalogSymbol> = pdg
            .node_indices()
            .filter_map(|index| pdg.get_node(index))
            .filter(|node| file_matches(root, &node.file_path, Some(&file_str)))
            .map(catalog_symbol_from_node)
            .collect();
        // SQL: ORDER BY byte_range_start, node_id LIMIT 200.
        symbols.sort_by(|a, b| {
            a.byte_range
                .0
                .cmp(&b.byte_range.0)
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        symbols.truncate(MAX_CATALOG_ROWS);
        symbols
    }

    /// Exact symbol count for one file, no cap.
    pub fn count_symbols_in_file(pdg: &ProgramDependenceGraph, root: &Path, file: &Path) -> usize {
        let file_str = relative_file(root, file);
        pdg.node_indices()
            .filter_map(|index| pdg.get_node(index))
            .filter(|node| file_matches(root, &node.file_path, Some(&file_str)))
            .count()
    }
}

/// Layer-backed equivalents of the graph queries (D6): resolve against the
/// published generation's Pdg layer WITHOUT a resident project — the
/// unhydrated MCP fast paths. `storage_root` is the project's storage root
/// (the directory holding `CURRENT`/`generations`/`cas` — NOT a generation
/// directory; `active_storage()` may point at either). Mirrors
/// `textindex::symbols_from_generation`; the SQL catalog remains the legacy
/// fallback in the callers.
pub mod layer {
    use super::*;

    /// Decode the published generation's Pdg layer at `storage_root`.
    /// `None` when no generation/layer exists or the payload is
    /// corrupt — callers fall back to the legacy SQL catalog.
    fn layer_graph(storage_root: &Path) -> Option<ProgramDependenceGraph> {
        let snapshot = crate::storage::generation::GenerationSnapshot::open(storage_root).ok()?;
        let reader = snapshot.pdg()?;
        reader.to_program_dependence_graph().ok()
    }

    /// [`graph::find_symbol`] over the published Pdg layer.
    pub fn find_symbol(
        storage_root: &Path,
        root: &Path,
        symbol: &str,
        file: Option<&Path>,
    ) -> Vec<CatalogSymbol> {
        let Some(pdg) = layer_graph(storage_root) else {
            return Vec::new();
        };
        super::graph::find_symbol(&pdg, root, symbol, file)
    }

    /// [`graph::symbols_in_file`] over the published Pdg layer.
    pub fn symbols_in_file(storage_root: &Path, root: &Path, file: &Path) -> Vec<CatalogSymbol> {
        let Some(pdg) = layer_graph(storage_root) else {
            return Vec::new();
        };
        super::graph::symbols_in_file(&pdg, root, file)
    }

    /// [`graph::count_symbols_in_file`] over the published Pdg layer.
    pub fn count_symbols_in_file(storage_root: &Path, root: &Path, file: &Path) -> usize {
        let Some(pdg) = layer_graph(storage_root) else {
            return 0;
        };
        super::graph::count_symbols_in_file(&pdg, root, file)
    }
}

/// A read-only catalog handle with one serialized SQLite connection reused by
/// all point lookups. Keeping the connection alive avoids reopening a large
/// catalog and rebuilding its page cache for every symbol/file request.
#[derive(Debug, Clone)]
pub struct CatalogReader {
    project_ids: [String; 2],
    connection: Arc<Mutex<Connection>>,
}

impl CatalogReader {
    /// Open an existing catalog for a canonical project path.
    pub async fn open(
        db_path: impl Into<PathBuf>,
        project_path: impl Into<PathBuf>,
    ) -> Result<Option<Self>> {
        let db_path = db_path.into();
        let project_path = project_path.into();
        tokio::task::spawn_blocking(move || {
            let connection = pooled_connection(&db_path)?;
            let conn = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("catalog connection poisoned"))?;
            let project_ids = conn
                .query_row(
                    "SELECT unique_project_id, base_name FROM project_metadata WHERE canonical_path = ?1",
                    [project_path.to_string_lossy().as_ref()],
                    |row| Ok([row.get::<_, String>(0)?, row.get::<_, String>(1)?]),
                )
                .optional()?;
            drop(conn);
            Ok(project_ids.map(|project_ids| Self {
                project_ids,
                connection,
            }))
        })
        .await
        .context("catalog lookup task failed")?
    }

    /// Find exact symbols first, then case-insensitive candidates, bounded to 200 rows.
    pub async fn find_symbol(
        &self,
        symbol: &str,
        file: Option<&Path>,
    ) -> Result<Vec<CatalogSymbol>> {
        let project_ids = self.project_ids.clone();
        let connection = self.connection.clone();
        let symbol = symbol.to_owned();
        let file = file.map(|path| path.to_string_lossy().into_owned());
        tokio::task::spawn_blocking(move || {
            let conn = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("catalog connection poisoned"))?;
            let mut statement = conn.prepare(
                "SELECT node_id, symbol_name, qualified_name, file_path, language, node_type, \
                        COALESCE(complexity, 0), COALESCE(byte_range_start, 0), COALESCE(byte_range_end, 0) \
                 FROM intel_nodes \
                 WHERE (project_id = ?1 OR project_id = ?2) \
                   AND (?3 IS NULL OR file_path = ?3) \
                   AND (symbol_name = ?4 OR qualified_name = ?4 \
                        OR symbol_name = ?4 COLLATE NOCASE OR qualified_name = ?4 COLLATE NOCASE) \
                 ORDER BY CASE WHEN symbol_name = ?4 OR qualified_name = ?4 THEN 0 ELSE 1 END, node_id \
                 LIMIT ?5",
            )?;
            rows(&mut statement, &[&project_ids[0], &project_ids[1], &file, &symbol, &(MAX_CATALOG_ROWS as i64)])
        })
        .await
        .context("catalog symbol lookup task failed")?
    }

    /// Return the hash recorded for one canonical source file, if the index
    /// has a freshness record for it.
    pub async fn indexed_file_hash(&self, file: &Path) -> Result<Option<String>> {
        let project_ids = self.project_ids.clone();
        let connection = self.connection.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("catalog connection poisoned"))?;
            Ok(conn
                .query_row(
                    "SELECT file_hash FROM indexed_files WHERE (project_id = ?1 OR project_id = ?2) AND file_path = ?3",
                    [&project_ids[0], &project_ids[1], &file],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })
        .await
        .context("catalog freshness lookup task failed")?
    }

    /// Return the bounded symbol inventory for a canonical file path.
    pub async fn symbols_in_file(&self, file: &Path) -> Result<Vec<CatalogSymbol>> {
        let project_ids = self.project_ids.clone();
        let connection = self.connection.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("catalog connection poisoned"))?;
            let mut statement = conn.prepare(
                "SELECT node_id, symbol_name, qualified_name, file_path, language, node_type, \
                        COALESCE(complexity, 0), COALESCE(byte_range_start, 0), COALESCE(byte_range_end, 0) \
                 FROM intel_nodes WHERE (project_id = ?1 OR project_id = ?2) AND file_path = ?3 \
                 ORDER BY byte_range_start, node_id LIMIT ?4",
            )?;
            rows(&mut statement, &[&project_ids[0], &project_ids[1], &file, &(MAX_CATALOG_ROWS as i64)])
        })
        .await
        .context("catalog file lookup task failed")?
    }

    /// Return the exact symbol count for a canonical file without the row cap.
    pub async fn count_symbols_in_file(&self, file: &Path) -> Result<usize> {
        let project_ids = self.project_ids.clone();
        let connection = self.connection.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("catalog connection poisoned"))?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM intel_nodes WHERE (project_id = ?1 OR project_id = ?2) AND file_path = ?3",
                [&project_ids[0], &project_ids[1], &file],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as usize)
        })
        .await
        .context("catalog file count task failed")?
    }
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open read-only catalog {}", path.display()))
}

fn pooled_connection(path: &Path) -> Result<Arc<Mutex<Connection>>> {
    let pool = CATALOG_CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pool = pool
        .lock()
        .map_err(|_| anyhow::anyhow!("catalog connection pool poisoned"))?;
    if let Some(connection) = pool.get(path) {
        return Ok(connection.clone());
    }
    let connection = Arc::new(Mutex::new(open_read_only(path)?));
    if pool.len() >= MAX_POOLED_CATALOG_CONNECTIONS {
        if let Some(oldest) = pool.keys().next().cloned() {
            pool.remove(&oldest);
        }
    }
    pool.insert(path.to_path_buf(), connection.clone());
    Ok(connection)
}

fn rows(
    statement: &mut rusqlite::Statement<'_>,
    values: &[&dyn rusqlite::ToSql],
) -> Result<Vec<CatalogSymbol>> {
    Ok(statement
        .query_map(values, |row| {
            Ok(CatalogSymbol {
                node_id: row.get(0)?,
                symbol_name: row.get(1)?,
                qualified_name: row.get(2)?,
                file_path: PathBuf::from(row.get::<_, String>(3)?),
                language: row.get(4)?,
                node_type: row.get(5)?,
                complexity: row.get::<_, i64>(6)?.max(0) as u32,
                byte_range: (
                    row.get::<_, i64>(7)?.max(0) as usize,
                    row.get::<_, i64>(8)?.max(0) as usize,
                ),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_connections_are_reused_by_path() {
        let temp = tempfile::tempdir().expect("catalog tempdir");
        let path = temp.path().join("catalog.db");
        Connection::open(&path).expect("create catalog db");
        let first = pooled_connection(&path).expect("first pooled connection");
        let second = pooled_connection(&path).expect("second pooled connection");
        assert!(Arc::ptr_eq(&first, &second));
    }
}
