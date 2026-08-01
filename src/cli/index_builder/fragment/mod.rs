// Fragment-level chunking for the localized content-hash store (1.11.0 plan).
//
// Tier-2 fragments are sub-symbol semantic chunks of a single PDG node's source
// range; Tier-3 fragments cover module-level orphan regions. Both are content-
// hash-addressed at embed time. The chunker is a localized port of Warp's
// `full_source_code_embedding` chunker (semantic tree-sitter split with a
// byte-safe naive line fallback), with two deliberate adaptations:
//
//   - offsets are plain `usize` (no `string_offset::ByteOffset`), and
//   - line spans are computed in-tree (no `line_span` crate) — zero new deps.
//
// Invariant: a fragment never crosses its owner node's byte range (Task 2
// invariant 5); Tier-3 orphan regions are computed as the complement of the
// Tier-1 node ranges and explicitly exclude the leading file-doc region.
//
// NOTE (dead_code): the module API (`chunk_code`, `orphan_fragments`,
// `enrich_fragment`, …) is the deliverable of Task 2 and is exercised by its
// own test module; production consumers land in Tasks 3–7 (fragment store,
// mmap persistence, query fusion). The `#[allow(dead_code)]` at the
// `mod fragment;` declaration in `index_builder/mod.rs` documents that planned
// rollout — not a suppressed defect.

use std::path::Path;

mod chunker;
mod enrich;
mod orphan;

// Re-exports so the task-local test module (tests.rs, included via
// `#[path]`) can exercise the chunker/enrich/orphan APIs through
// `use super::*`. Gated on `#[cfg(test)]` so non-test builds emit no
// unused-import warnings (the production consumers land in Tasks 3-7).
#[cfg(test)]
pub(crate) use chunker::{chunk_naive, chunk_semantic};
#[cfg(test)]
pub(crate) use enrich::{enrich_fragment, enrich_orphan, orphan_header, owner_header};
#[cfg(test)]
pub(crate) use orphan::{OrphanInput, orphan_fragments};

/// Number of lines per chunk when chunking naively (≈ Warp's `LINES_PER_CHUNK`).
const LINES_PER_CHUNK: usize = 200;

/// Average number of characters per line (≈ Warp's `AVG_CHAR_PER_LINE`).
const AVG_CHAR_PER_LINE: usize = 60;

/// Default max bytes per fragment — 200 lines × 60 chars ≈ Warp's default and
/// the `[search] fragment_max_bytes` config default (12_000).
pub(crate) const MAX_BYTES_PER_CHUNK: usize = LINES_PER_CHUNK * AVG_CHAR_PER_LINE;

/// A code fragment with line + byte range information.
///
/// Byte offsets are into the file/region `&str` the fragment borrows from
/// (file-absolute for whole-file chunking; region-relative for orphan regions,
/// which are re-based by the caller before surfacing).
#[derive(Debug, Clone)]
pub(crate) struct Fragment<'a> {
    /// The content of the fragment.
    pub(crate) content: &'a str,
    /// Start line number (inclusive).
    pub(crate) start_line: usize,
    /// End line number (inclusive).
    pub(crate) end_line: usize,
    /// Start byte index of the fragment in the original source.
    pub(crate) start_byte_index: usize,
    /// End byte index (exclusive) of the fragment in the original source.
    pub(crate) end_byte_index: usize,
    /// File path of the fragment.
    pub(crate) file_path: &'a Path,
}

impl<'a> Fragment<'a> {
    fn size(&self) -> usize {
        self.content.len()
    }

    fn append(&mut self, other: &Fragment<'a>, content: &'a str) {
        self.end_line = other.end_line;
        self.end_byte_index = other.end_byte_index;
        self.content = &content[self.start_byte_index..other.end_byte_index];
    }
}

/// Coalesce small fragments into larger ones that still respect `max_bytes_per_chunk`.
///
/// Tree-sitter often produces small fragments that split function names from
/// the actual function body; we iterate in reverse to coalesce these chunks
/// into fragments that are more meaningful. Ported from Warp's chunker.
fn coalesce_fragments<'a>(
    fragments: impl DoubleEndedIterator<Item = Fragment<'a>>,
    code: &'a str,
    max_bytes_per_chunk: usize,
) -> Vec<Fragment<'a>> {
    fragments
        .rev()
        .fold(
            Vec::new(),
            |mut acc: Vec<Fragment<'a>>, mut fragment| match acc.last_mut() {
                Some(last_item) => {
                    let new_fragment_size =
                        code[fragment.start_byte_index..last_item.end_byte_index].len();
                    if new_fragment_size <= max_bytes_per_chunk {
                        fragment.append(last_item, code);
                        *last_item = fragment;
                    } else {
                        acc.push(fragment);
                    }
                    acc
                }
                None => {
                    acc.push(fragment);
                    acc
                }
            },
        )
        .into_iter()
        .rev()
        .collect()
}

/// Chunks code into an ordered list of fragments.
///
/// The code is chunked "semantically" using tree-sitter when a grammar is
/// available for the file's extension; otherwise fragments are naively chunked
/// by lines (byte-safe splits, 200 lines per chunk by default). Note that the
/// grammar registry is extension-keyed, so extensionless files (`Makefile`,
/// `Dockerfile`, `.gitignore`) intentionally fall back to naive chunking.
///
/// NOTE for Tasks 3-7: consume `chunker::chunk_naive` / `chunker::chunk_semantic`
/// / `orphan_fragments` via full paths or this entry point — the mod.rs
/// re-exports are `#[cfg(test)]`-gated and do not exist in non-test builds.
pub(crate) fn chunk_code<'a>(code: &'a str, path: &'a Path) -> Vec<Fragment<'a>> {
    if let Some(fragments) = try_chunk_code_semantically(code, path) {
        return fragments;
    }
    chunker::chunk_naive(code, path, MAX_BYTES_PER_CHUNK, LINES_PER_CHUNK)
}

/// Attempts to chunk code semantically, returning `None` when no grammar exists
/// for the file extension or the parse/split fails (caller falls back to naive).
fn try_chunk_code_semantically<'a>(code: &'a str, path: &'a Path) -> Option<Vec<Fragment<'a>>> {
    let ext = path.extension()?.to_str()?;
    let language_id = crate::parse::grammar::LanguageId::from_extension(ext)?;
    let language = language_id.from_cache().ok()?;
    chunker::chunk_semantic(code, path, MAX_BYTES_PER_CHUNK, &language).ok()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
