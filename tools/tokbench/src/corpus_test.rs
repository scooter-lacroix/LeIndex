//! Corpus pinning tests: hash stability and deterministic materialization.

use super::*;

#[test]
fn test_corpus_hash_is_stable() {
    // Called twice in the same process and pinned by construction: the hash
    // must be a 64-hex digest and identical across calls (SOURCE_FILES is a
    // const, so any change is a deliberate corpus re-pin).
    let a = corpus_hash();
    let b = corpus_hash();
    assert_eq!(a, b);
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_materialize_writes_every_pinned_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    materialize(dir.path()).expect("materialize");
    for (relative, content) in SOURCE_FILES {
        let path = dir.path().join(relative);
        let on_disk =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing {relative}: {e}"));
        assert_eq!(on_disk, *content, "content drift in {relative}");
    }
}

#[test]
fn test_fixture_has_no_option_string_question_mark_brace() {
    // Task 7's zero-result search depends on this invariant.
    let hit = SOURCE_FILES
        .iter()
        .any(|(_, content)| content.contains("Option<String>?{"));
    assert!(!hit, "control literal leaked into the corpus");
}
