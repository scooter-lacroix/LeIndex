//! Project text-index lifecycle: locate, load (cached), build, and feed it
//! symbol spans from the generation catalog — never from a hydrated PDG.

use crate::search::textsearch::{
    BuildStats, SymbolSpan, TextIndex, build_index, kind_code, list_files,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use tracing::{debug, info, warn};

/// Projects with more files than this are not indexed inline on first use
/// (a background build is started instead and the call scans live).
pub const INLINE_BUILD_MAX_FILES: usize = 20_000;

/// Location of a project's index inside its storage root.
pub fn index_path(storage: &Path) -> PathBuf {
    storage.join("textindex").join("index.bin")
}

type Cache = Mutex<HashMap<PathBuf, (SystemTime, Arc<TextIndex>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Load the index at `storage`, reusing the mapped copy while the file is
/// unchanged. A rebuilt index (atomic rename) has a new mtime and is reloaded.
pub fn load(storage: &Path) -> Option<Arc<TextIndex>> {
    let path = index_path(storage);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    if let Ok(guard) = cache().lock() {
        if let Some((seen, index)) = guard.get(&path) {
            if *seen == modified {
                return Some(Arc::clone(index));
            }
        }
    }
    match TextIndex::open(&path) {
        Ok(index) => {
            let index = Arc::new(index);
            if let Ok(mut guard) = cache().lock() {
                guard.insert(path, (modified, Arc::clone(&index)));
            }
            Some(index)
        }
        Err(error) => {
            warn!("Ignoring unreadable text index {}: {error}", path.display());
            None
        }
    }
}

/// Symbol spans per root-relative path, read from the generation's
/// `intel_nodes` table (read-only, no PDG involved).
pub fn symbols_from_db(db: &Path, root: &Path) -> HashMap<String, Vec<SymbolSpan>> {
    let mut out: HashMap<String, Vec<SymbolSpan>> = HashMap::new();
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(connection) = rusqlite::Connection::open_with_flags(db, flags) else {
        return out;
    };
    let _ = connection.busy_timeout(std::time::Duration::from_secs(2));
    let Ok(mut statement) = connection.prepare(
        "SELECT file_path, symbol_name, node_type, byte_range_start, byte_range_end \
         FROM intel_nodes WHERE byte_range_end > byte_range_start",
    ) else {
        return out;
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    });
    let Ok(rows) = rows else { return out };
    for (file, name, kind, start, end) in rows.flatten() {
        let path = Path::new(&file);
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.entry(rel).or_default().push(SymbolSpan {
            start: start.max(0) as u32,
            end: end.max(0) as u32,
            kind: kind_code(&kind),
            name,
        });
    }
    out
}

/// Build (or rebuild) the index for `root` into `storage`.
pub fn build(root: &Path, storage: &Path, db: Option<&Path>) -> std::io::Result<BuildStats> {
    let symbols = db.map(|db| symbols_from_db(db, root)).unwrap_or_default();
    let stats = build_index(root, &index_path(storage), symbols)?;
    info!(
        root = %root.display(),
        files = stats.files,
        trigrams = stats.trigrams,
        bytes = stats.bytes,
        millis = stats.millis as u64,
        "Text index built"
    );
    Ok(stats)
}

fn building() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static SET: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

/// The index for a project, building it when that is cheap.
///
/// Returns `None` (the caller scans live) when the project has never been
/// indexed — no storage directory exists and none is created here — or when it
/// is too large to index inline, in which case a detached build is started so
/// later calls are fast.
pub fn ensure(root: &Path, storage: &Path, active_storage: &Path) -> Option<Arc<TextIndex>> {
    if let Some(index) = load(storage) {
        return Some(index);
    }
    if !storage.is_dir() {
        return None;
    }
    let db = active_storage.join("leindex.db");
    let db = db.is_file().then_some(db);
    let file_count = list_files(root, INLINE_BUILD_MAX_FILES + 1, None).len();
    if file_count <= INLINE_BUILD_MAX_FILES {
        if let Err(error) = build(root, storage, db.as_deref()) {
            warn!("Text index build failed for {}: {error}", root.display());
            return None;
        }
        return load(storage);
    }
    let key = root.to_path_buf();
    if !building()
        .lock()
        .is_ok_and(|mut set| set.insert(key.clone()))
    {
        return None;
    }
    let (root, storage) = (root.to_path_buf(), storage.to_path_buf());
    std::thread::spawn(move || {
        if let Err(error) = build(&root, &storage, db.as_deref()) {
            warn!(
                "Background text index build failed for {}: {error}",
                root.display()
            );
        }
        if let Ok(mut set) = building().lock() {
            set.remove(&key);
        }
    });
    debug!("Large project: text index building in the background");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_never_creates_storage_for_an_unindexed_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn x() {}").unwrap();
        let storage = dir.path().join(".leindex");
        assert!(ensure(dir.path(), &storage, &storage).is_none());
        assert!(
            !storage.exists(),
            "searching must not litter an unindexed project"
        );
    }

    #[test]
    fn test_ensure_builds_once_storage_exists_and_reloads_after_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha_marker() {}").unwrap();
        let storage = dir.path().join(".leindex");
        std::fs::create_dir_all(&storage).unwrap();
        let first = ensure(dir.path(), &storage, &storage).expect("built inline");
        assert_eq!(first.file_count(), 1);
        let again = ensure(dir.path(), &storage, &storage).unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "unchanged file reuses the mapping"
        );

        std::fs::write(dir.path().join("b.rs"), "fn beta_marker() {}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        build(dir.path(), &storage, None).unwrap();
        assert_eq!(load(&storage).unwrap().file_count(), 2);
    }

    #[test]
    fn test_symbols_from_db_tolerates_missing_or_foreign_databases() {
        let dir = tempfile::tempdir().unwrap();
        assert!(symbols_from_db(&dir.path().join("nope.db"), dir.path()).is_empty());
        let db = dir.path().join("x.db");
        std::fs::write(&db, b"not sqlite").unwrap();
        assert!(symbols_from_db(&db, dir.path()).is_empty());
    }

    #[test]
    fn test_symbols_from_db_reads_intel_nodes_and_relativizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("g.db");
        {
            let connection = rusqlite::Connection::open(&db).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE intel_nodes (file_path TEXT, symbol_name TEXT, node_type TEXT, byte_range_start INTEGER, byte_range_end INTEGER);",
                )
                .unwrap();
            let file = dir.path().join("src/a.rs");
            connection
                .execute(
                    "INSERT INTO intel_nodes VALUES (?1, 'alpha', 'Function', 3, 40), (?1, 'skip', 'Function', 0, 0)",
                    [file.to_string_lossy().as_ref()],
                )
                .unwrap();
        }
        let symbols = symbols_from_db(&db, dir.path());
        let spans = &symbols["src/a.rs"];
        assert_eq!(spans.len(), 1, "empty ranges are ignored");
        assert_eq!(
            (spans[0].name.as_str(), spans[0].start, spans[0].end),
            ("alpha", 3, 40)
        );
    }
}
