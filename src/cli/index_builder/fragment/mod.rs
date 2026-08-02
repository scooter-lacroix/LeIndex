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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod chunker;
mod enrich;
pub(crate) mod extract;
mod orphan;
pub(crate) mod sync;

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
#[cfg(test)]
pub(crate) use sync::{
    FragmentCandidate, FragmentFileManifest, compute_fragment_root_hash,
    fragment_layer_generation_is_consistent, incremental_sync_fragments, load_fragment_root,
    load_fragment_sync_manifest, persist_fragment_root, persist_fragment_sync_manifest,
};

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
    if let Some(mut fragments) = try_chunk_code_semantically(code, path) {
        // ponytail: empty-file edge case can yield one spurious empty fragment
        // via the semantic path (naive path already returns []). Drop it so the
        // store never embeds/hashes an empty string. O(n) retain, n = fragments
        // per file — negligible vs the embed round-trip.
        fragments.retain(|f| !f.content.is_empty());
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

// ---------------------------------------------------------------------------
// Content-hash fragment store (Task 3)
// ---------------------------------------------------------------------------

/// Schema version for the fragment store bincode artifact (mirrors the
/// `TFIDF_SCHEMA_VERSION` discipline). Bump when the persisted layout changes;
/// stale stores are rejected on load, never silently reused.
const FRAGMENT_STORE_SCHEMA_VERSION: u32 = 1;

/// Metadata for one fragment row.
///
/// `content_hash` is the blake3 hex of the exact enriched text that was
/// embedded (invariant 3: cache key ≡ embedding input). Multiple metadata refs
/// may share one content hash — the dedup invariant: identical text is
/// embedded once and referenced N times.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FragmentMetadata {
    /// blake3(enriched text that was embedded).
    pub(crate) content_hash: String,
    /// Tier-1 node id when the fragment lives inside a symbol; `None` for
    /// Tier-3 orphan (module-level) fragments.
    pub(crate) owner: Option<String>,
    /// File path of the fragment's source file.
    pub(crate) file_path: String,
    /// Exact source byte range (file-absolute).
    pub(crate) byte_range: (usize, usize),
    /// Source line range (inclusive, 0-based like tree-sitter rows).
    pub(crate) line_range: (usize, usize),
    /// Row offset into the fragment embeddings mmap (filled at persist time).
    pub(crate) embedding_offset: u64,
}

/// Persisted fragment store state (bincode at `.leindex/fragment_store.bin`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FragmentStoreState {
    #[serde(default)]
    schema_version: u32,
    /// content_hash → metadata refs (one embedding row, N refs).
    #[serde(default)]
    rows: HashMap<String, Vec<FragmentMetadata>>,
}

/// Content-hash-addressed fragment store with dedup.
#[derive(Debug, Clone, Default)]
pub(crate) struct FragmentStore {
    rows: HashMap<String, Vec<FragmentMetadata>>,
}

impl FragmentStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of unique embedding rows (distinct content hashes).
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Insert a fragment row, deduplicating by content hash.
    pub(crate) fn insert(&mut self, meta: FragmentMetadata) {
        self.rows
            .entry(meta.content_hash.clone())
            .or_default()
            .push(meta);
    }

    /// Metadata refs for a content hash (`None` when unknown).
    pub(crate) fn get(&self, content_hash: &str) -> Option<&[FragmentMetadata]> {
        self.rows.get(content_hash).map(Vec::as_slice)
    }

    /// Remove a content-hash row and ALL of its metadata refs.
    ///
    /// Used by incremental sync (Task 7) when a file is removed or re-chunked:
    /// the embedding row is dropped when no refs remain.
    pub(crate) fn remove_hash(&mut self, content_hash: &str) {
        self.rows.remove(content_hash);
    }

    /// All content hashes (embedding row keys).
    pub(crate) fn content_hashes(&self) -> impl Iterator<Item = &str> {
        self.rows.keys().map(String::as_str)
    }

    /// Owner-node mapping: owner node id → content hashes of its fragments
    /// (invariant 6: fragment hits map back to owner nodes before surfacing).
    pub(crate) fn owner_to_hashes(&self) -> HashMap<String, Vec<String>> {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for (hash, metas) in &self.rows {
            for meta in metas {
                if let Some(owner) = &meta.owner {
                    out.entry(owner.clone()).or_default().push(hash.clone());
                }
            }
        }
        out
    }

    /// Total fragment count (metadata refs, not unique embedding rows).
    pub(crate) fn fragment_count(&self) -> usize {
        self.rows.values().map(Vec::len).sum()
    }

    fn storage_path(project_path: &Path) -> PathBuf {
        project_path.join(".leindex").join("fragment_store.bin")
    }

    /// Load the fragment store from storage (None when absent or schema-mismatched).
    pub(crate) fn load_from_storage(project_path: &Path) -> Result<Option<Self>> {
        Self::load_from_artifact_path(&project_path.join(".leindex"))
    }

    /// Load from an explicit storage directory (generation hydration path).
    pub(crate) fn load_from_artifact_path(storage_path: &Path) -> Result<Option<Self>> {
        let path = storage_path.join("fragment_store.bin");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Failed to read fragment store: {}", path.display()))?;
        let state: FragmentStoreState = bincode::deserialize(&bytes)
            .with_context(|| format!("Failed to deserialize fragment store: {}", path.display()))?;
        Ok(Self::from_persisted_state(state))
    }

    /// Persist the store to `.leindex/fragment_store.bin`.
    pub(crate) fn persist_to_storage(&self, project_path: &Path) -> Result<()> {
        let path = Self::storage_path(project_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create fragment store directory: {}",
                    parent.display()
                )
            })?;
        }
        let payload = bincode::serialize(&FragmentStoreState {
            schema_version: FRAGMENT_STORE_SCHEMA_VERSION,
            rows: self.rows.clone(),
        })
        .context("Failed to serialize fragment store")?;
        std::fs::write(&path, payload)
            .with_context(|| format!("Failed to persist fragment store: {}", path.display()))
    }

    fn from_persisted_state(state: FragmentStoreState) -> Option<Self> {
        if state.schema_version != FRAGMENT_STORE_SCHEMA_VERSION {
            tracing::warn!(
                "Persisted fragment store schema version {} != current {}; discarding",
                state.schema_version,
                FRAGMENT_STORE_SCHEMA_VERSION
            );
            return None;
        }
        Some(Self { rows: state.rows })
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
