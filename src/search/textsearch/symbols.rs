//! Symbol-definition search over the trigram index.
//!
//! Split from `engine` (the 2000-line gate): symbols come straight from the
//! index's symbol table — no PDG, no parsing — while the byte-to-line
//! conversion reads the live file under the same containment rules the
//! scanner enforces for index-seeded paths.

use super::engine::{Compiled, RootSpec, dirty_files, path_stays_inside};

/// A symbol whose *name* matched.
#[derive(Debug, Clone)]
pub struct SymbolHit {
    /// Root index in the `roots` slice.
    pub root: usize,
    /// Root-relative file.
    pub rel: String,
    /// Symbol name.
    pub name: String,
    /// Kind (`function`, `class`, ...).
    pub kind: &'static str,
    /// First line (1-based); `0` when the file could not be read.
    pub line: u32,
    /// Last line (1-based).
    pub end_line: u32,
    /// The file changed after indexing, so the line numbers may be off.
    pub stale: bool,
    /// 0 exact name, 1 prefix, 2 other.
    pub rank: u8,
}

/// Find symbol definitions by name across indexed roots. Symbols come from the
/// index, so this needs no PDG and no parsing; exact matches sort first.
/// Returns `(window, total)`.
pub fn search_symbols(
    roots: &[RootSpec],
    compiled: &Compiled,
    pattern_lower: &str,
    kinds: &[String],
    offset: usize,
    limit: Option<usize>,
) -> (Vec<SymbolHit>, usize) {
    let mut hits: Vec<SymbolHit> = Vec::new();
    for (root_id, spec) in roots.iter().enumerate() {
        let Some(index) = &spec.index else { continue };
        let dirty_list = dirty_files(&spec.root, index);
        let dirty: std::collections::HashSet<&str> =
            dirty_list.iter().map(String::as_str).collect();
        for id in index.file_ids() {
            let Some(meta) = index.file(id) else { continue };
            if !spec.filter.allows(meta.path) {
                continue;
            }
            for (name, kind, start, end) in index.symbols(id) {
                if !kinds.is_empty() && !kinds.iter().any(|k| k.eq_ignore_ascii_case(kind)) {
                    continue;
                }
                if !compiled.is_match(name.as_bytes()) {
                    continue;
                }
                let lowered = name.to_ascii_lowercase();
                let rank = if lowered == pattern_lower {
                    0
                } else if lowered.starts_with(pattern_lower) {
                    1
                } else {
                    2
                };
                hits.push(SymbolHit {
                    root: root_id,
                    rel: meta.path.to_string(),
                    name,
                    kind,
                    line: start,
                    end_line: end,
                    stale: dirty.contains(meta.path),
                    rank,
                });
            }
        }
    }
    hits.sort_by(|a, b| (a.rank, a.root, &a.rel, a.line).cmp(&(b.rank, b.root, &b.rel, b.line)));
    let total = hits.len();
    let mut window: Vec<SymbolHit> = hits
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    // Byte offsets -> lines, only for the returned window. The read is
    // seeded from the index (`meta.path`), so it gets the same revalidation
    // as `scan_file`: a path re-pointed at a symlink outside the root is not
    // read, and a non-regular entry (e.g. a FIFO substituted since indexing)
    // is never opened — `fs::read` would block on it with no deadline.
    let roots_canonical: Vec<Option<std::path::PathBuf>> = roots
        .iter()
        .map(|s| std::fs::canonicalize(&s.root).ok())
        .collect();
    for hit in &mut window {
        let path = roots[hit.root].root.join(&hit.rel);
        let data = path_stays_inside(&path, roots_canonical[hit.root].as_deref())
            .then(|| std::fs::metadata(&path).ok())
            .flatten()
            .filter(|meta| meta.is_file())
            .and_then(|_| std::fs::read(&path).ok());
        if let Some(data) = data {
            let (start, end) = (hit.line as usize, hit.end_line as usize);
            let line_of = |at: usize| {
                1 + memchr::memchr_iter(b'\n', &data[..at.min(data.len())]).count() as u32
            };
            hit.line = line_of(start);
            hit.end_line = line_of(end.saturating_sub(1).max(start));
        } else {
            hit.line = 0;
            hit.end_line = 0;
        }
    }
    (window, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::textsearch::{SymbolSpan, TextIndex};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    fn spec(root: &Path, index: Option<Arc<TextIndex>>) -> RootSpec {
        RootSpec {
            root: root.to_path_buf(),
            index,
            filter: crate::search::textsearch::FileFilter::new(&[], &[], None),
        }
    }

    /// The symbol route reads index-seeded paths to convert byte offsets to
    /// lines. A file swapped for a symlink pointing outside the root since
    /// indexing must not be followed: the hit survives (symbols come from
    /// the index) with line 0, never the outside file's line numbers.
    #[test]
    #[cfg(unix)]
    fn test_symbol_lines_refuse_out_of_root_symlink_swap() {
        let dir = tempfile::tempdir().unwrap();
        let text = "line one\nfn parse() {}\n";
        std::fs::create_dir_all(dir.path().join(".leindex")).unwrap();
        std::fs::write(dir.path().join("a.rs"), text).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("secret.txt"),
            "\n\n\n\n\n\n\n\n\n\nfn parse() {}\n",
        )
        .unwrap();

        let start = text.find("fn parse()").unwrap() as u32;
        let mut symbols = HashMap::new();
        symbols.insert(
            "a.rs".to_string(),
            vec![SymbolSpan {
                start,
                end: start + 11,
                kind: 0,
                name: "parse".into(),
            }],
        );
        let out = dir.path().join(".leindex/textindex/index.bin");
        crate::search::textsearch::build_index(dir.path(), &out, symbols).unwrap();
        let specs = [spec(
            dir.path(),
            Some(Arc::new(TextIndex::open(&out).unwrap())),
        )];
        let compiled = crate::search::textsearch::Query {
            pattern: "parse".into(),
            regex: false,
            case: Default::default(),
            word: false,
        }
        .compile()
        .unwrap();
        let (hits, _) = search_symbols(&specs, &compiled, "parse", &[], 0, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2, "sanity: real file resolves the line");

        std::fs::remove_file(dir.path().join("a.rs")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("a.rs"))
            .unwrap();
        let (hits, _) = search_symbols(&specs, &compiled, "parse", &[], 0, None);
        assert_eq!(
            hits.len(),
            1,
            "the symbol itself still comes from the index"
        );
        assert_eq!(
            (hits[0].line, hits[0].end_line),
            (0, 0),
            "the out-of-root read must be refused, not followed"
        );
    }
}
