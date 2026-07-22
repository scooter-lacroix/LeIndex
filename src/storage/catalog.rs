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
    project_id: String,
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
            let project_id = conn
                .query_row(
                    "SELECT unique_project_id FROM project_metadata WHERE canonical_path = ?1",
                    [project_path.to_string_lossy().as_ref()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            Ok(project_id.map(|project_id| Self {
                db_path,
                project_id,
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
        let project_id = self.project_id.clone();
        let symbol = symbol.to_owned();
        let file = file.map(|path| path.to_string_lossy().into_owned());
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
            let mut statement = conn.prepare(
                "SELECT node_id, symbol_name, qualified_name, file_path, language, node_type, \
                        COALESCE(complexity, 0), COALESCE(byte_range_start, 0), COALESCE(byte_range_end, 0) \
                 FROM intel_nodes \
                 WHERE project_id = ?1 \
                   AND (?2 IS NULL OR file_path = ?2) \
                   AND (symbol_name = ?3 OR qualified_name = ?3 \
                        OR symbol_name = ?3 COLLATE NOCASE OR qualified_name = ?3 COLLATE NOCASE) \
                 ORDER BY CASE WHEN symbol_name = ?3 OR qualified_name = ?3 THEN 0 ELSE 1 END, node_id \
                 LIMIT ?4",
            )?;
            rows(&mut statement, &[&project_id, &file, &symbol, &(MAX_CATALOG_ROWS as i64)])
        })
        .await
        .context("catalog symbol lookup task failed")?
    }

    /// Return the bounded symbol inventory for a canonical file path.
    pub async fn symbols_in_file(&self, file: &Path) -> Result<Vec<CatalogSymbol>> {
        let db_path = self.db_path.clone();
        let project_id = self.project_id.clone();
        let file = file.to_string_lossy().into_owned();
        tokio::task::spawn_blocking(move || {
            let conn = open_read_only(&db_path)?;
            let mut statement = conn.prepare(
                "SELECT node_id, symbol_name, qualified_name, file_path, language, node_type, \
                        COALESCE(complexity, 0), COALESCE(byte_range_start, 0), COALESCE(byte_range_end, 0) \
                 FROM intel_nodes WHERE project_id = ?1 AND file_path = ?2 \
                 ORDER BY byte_range_start, node_id LIMIT ?3",
            )?;
            rows(&mut statement, &[&project_id, &file, &(MAX_CATALOG_ROWS as i64)])
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
