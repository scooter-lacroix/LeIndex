//! Project text-index lifecycle: locate, load (cached), build, and feed it
//! symbol spans decoded from the published generation's Pdg layer.

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

/// Symbol spans per root-relative path, decoded from the published
/// generation's Pdg layer — the sole graph store after the write flip (the
/// catalog no longer carries graph rows). `storage` is the project's storage
/// root (where `<storage>/generations/<CURRENT>` lives); byte ranges come from
/// the graph nodes and empty ranges are ignored.
pub fn symbols_from_generation(storage: &Path, root: &Path) -> HashMap<String, Vec<SymbolSpan>> {
    let Ok(snapshot) = crate::storage::generation::GenerationSnapshot::open(storage) else {
        return HashMap::new();
    };
    let Some(reader) = snapshot.pdg() else {
        return HashMap::new();
    };
    let Ok(pdg) = reader.to_program_dependence_graph() else {
        return HashMap::new();
    };
    symbols_from_pdg(&pdg, root)
}

/// Symbol spans (byte ranges) per root-relative path from an in-memory graph.
fn symbols_from_pdg(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    root: &Path,
) -> HashMap<String, Vec<SymbolSpan>> {
    let mut out: HashMap<String, Vec<SymbolSpan>> = HashMap::new();
    for index in pdg.node_indices() {
        let Some(node) = pdg.get_node(index) else {
            continue;
        };
        let (start, end) = node.byte_range;
        if end <= start {
            continue;
        }
        let path = Path::new(&*node.file_path);
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel = rel.to_string_lossy().replace('\\', "/");
        out.entry(rel).or_default().push(SymbolSpan {
            start: start as u32,
            end: end as u32,
            kind: kind_code(
                crate::storage::generation::graph_codec::graph_node_type_str(&node.node_type),
            ),
            name: node.name.clone(),
        });
    }
    out
}

/// Build (or rebuild) the index for `root` into `storage`.
pub fn build(root: &Path, storage: &Path) -> std::io::Result<BuildStats> {
    let symbols = symbols_from_generation(storage, root);
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
pub fn ensure(root: &Path, storage: &Path) -> Option<Arc<TextIndex>> {
    if let Some(index) = load(storage) {
        return Some(index);
    }
    if !storage.is_dir() {
        return None;
    }
    let file_count = list_files(root, INLINE_BUILD_MAX_FILES + 1, None).len();
    if file_count <= INLINE_BUILD_MAX_FILES {
        if let Err(error) = build(root, storage) {
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
        if let Err(error) = build(&root, &storage) {
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
        assert!(ensure(dir.path(), &storage).is_none());
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
        let first = ensure(dir.path(), &storage).expect("built inline");
        assert_eq!(first.file_count(), 1);
        let again = ensure(dir.path(), &storage).unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "unchanged file reuses the mapping"
        );

        std::fs::write(dir.path().join("b.rs"), "fn beta_marker() {}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        build(dir.path(), &storage).unwrap();
        assert_eq!(load(&storage).unwrap().file_count(), 2);
    }

    #[test]
    fn test_symbols_from_generation_is_empty_without_a_layer() {
        let dir = tempfile::tempdir().unwrap();
        // A storage root with no generation store yields no symbol spans.
        assert!(symbols_from_generation(dir.path(), dir.path()).is_empty());
    }

    #[test]
    fn test_symbols_from_pdg_relativizes_paths_and_skips_empty_ranges() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
        use std::sync::Arc;

        let root = Path::new("/proj");
        let mut pdg = ProgramDependenceGraph::new();
        let file: Arc<str> = Arc::from("/proj/src/a.rs");
        pdg.add_node(Node {
            id: "fn:alpha".to_string(),
            node_type: NodeType::Function,
            name: "alpha".to_string(),
            file_path: Arc::clone(&file),
            byte_range: (3, 40),
            complexity: 1,
            language: "rust".to_string(),
        });
        pdg.add_node(Node {
            id: "fn:skip".to_string(),
            node_type: NodeType::Function,
            name: "skip".to_string(),
            file_path: file,
            byte_range: (0, 0),
            complexity: 1,
            language: "rust".to_string(),
        });

        let symbols = symbols_from_pdg(&pdg, root);
        let spans = &symbols["src/a.rs"];
        assert_eq!(spans.len(), 1, "empty ranges are ignored");
        assert_eq!(
            (spans[0].name.as_str(), spans[0].start, spans[0].end),
            ("alpha", 3, 40)
        );
    }
}
