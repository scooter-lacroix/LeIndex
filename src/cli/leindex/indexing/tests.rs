use super::*;

#[test]
fn watcher_delta_publishes_current_generation() {
    let temp = tempfile::tempdir().expect("watcher fixture");
    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    let source = temp.path().join("src/lib.rs");
    std::fs::write(&source, "pub fn watcher_marker() -> usize { 1 }\n").expect("initial source");

    let mut index = LeIndex::new(temp.path()).expect("create index");
    index.index_project(true).expect("initial generation");
    let storage = temp.path().join(".leindex");
    let initial = std::fs::read_to_string(storage.join("CURRENT"))
        .expect("initial CURRENT")
        .trim()
        .parse::<u64>()
        .expect("initial generation number");

    std::fs::write(&source, "pub fn watcher_marker() -> usize { 2 }\n").expect("changed source");
    index
        .incremental_reindex_from_watcher()
        .expect("watcher delta");

    let published = std::fs::read_to_string(storage.join("CURRENT"))
        .expect("published CURRENT")
        .trim()
        .parse::<u64>()
        .expect("published generation number");
    assert!(published > initial);
    assert!(storage
        .join("generations")
        .join(published.to_string())
        .join("leindex.db")
        .is_file());
}
