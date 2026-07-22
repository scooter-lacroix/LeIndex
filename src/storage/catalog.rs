//! Bounded, read-only point lookups over persisted index nodes.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::{Path, PathBuf};

const MAX_CATALOG_ROWS: usize = 200;

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

/// A connection-free catalog handle. SQLite connections are opened only inside
/// the blocking operations that use them.
#[derive(Debug, Clone)]
pub struct CatalogReader {
    db_path: PathBuf,
    project_ids: [String; 2],
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
            let conn = open_read_only(&db_path)?;
            let project_ids = conn
                .query_row(
                    "SELECT unique_project_id, base_name FROM project_metadata WHERE canonical_path = ?1",
                    [project_path.to_string_lossy().as_ref()],
                    |row| Ok([row.get::<_, String>(0)?, row.get::<_, String>(1)?]),
                )
                .optional()?;
            Ok(project_ids.map(|project_ids| Self {
                db_path,
                project_ids,
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
        let db_path = self.db_path.clone();
        let project_ids = self.project_ids.clone();
        let symbol = symbol.to_owned();
        let file = file.map(|path| path.to_string_lossy().into_owned());
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
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

    /// Find literal symbol-name matches in deterministic exact-match order.
    pub async fn find_symbols_matching(&self, pattern: &str) -> Result<Vec<CatalogSymbol>> {
        let db_path = self.db_path.clone();
        let project_ids = self.project_ids.clone();
        let pattern = pattern.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
            let mut statement = conn.prepare(
                "SELECT node_id, symbol_name, qualified_name, file_path, language, node_type, \
                        COALESCE(complexity, 0), COALESCE(byte_range_start, 0), COALESCE(byte_range_end, 0) \
                 FROM intel_nodes \
                 WHERE (project_id = ?1 OR project_id = ?2) \
                   AND (symbol_name LIKE '%' || ?3 || '%' \
                        OR qualified_name LIKE '%' || ?3 || '%' \
                        OR symbol_name LIKE '%' || ?3 || '%' COLLATE NOCASE \
                        OR qualified_name LIKE '%' || ?3 || '%' COLLATE NOCASE) \
                 ORDER BY CASE \
                    WHEN qualified_name = ?3 THEN 0 \
                    WHEN symbol_name = ?3 THEN 1 \
                    WHEN qualified_name = ?3 COLLATE NOCASE THEN 2 \
                    ELSE 3 \
                 END, node_id \
                 LIMIT ?4",
            )?;
            rows(
                &mut statement,
                &[&project_ids[0], &project_ids[1], &pattern, &(MAX_CATALOG_ROWS as i64)],
            )
        })
        .await
        .context("catalog symbol search task failed")?
    }

    /// Return the hash recorded for one canonical source file, if the index
    /// has a freshness record for it.
    pub async fn indexed_file_hash(&self, file: &Path) -> Result<Option<String>> {
        let db_path = self.db_path.clone();
        let project_ids = self.project_ids.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
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
        let db_path = self.db_path.clone();
        let project_ids = self.project_ids.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
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
}

fn open_read_only(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open read-only catalog {}", path.display()))
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
