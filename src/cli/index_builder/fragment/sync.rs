//! Fragment sync: root-hash computation, generation-state persistence, and
//! the incremental per-file sync engine (fragment-embeddings 1.11.0 Tasks 3
//! and 7).
//!
//! The fragment root hash is a blake3 digest over the sorted set of
//! `content_hash × embedding-schema-version` pairs, so any change to a
//! fragment's embedded text (content hash) — or to the enrichment format
//! (schema version) — changes the root. Hydration rejects a fragment mmap
//! whose row count or root hash mismatches the persisted root (invariant 8),
//! mirroring the `restore_from_search_snapshot` discipline. Persisted at
//! `.leindex/fragment_root.bin` with a monotonic generation counter.
//!
//! Task 7 adds the incremental engine: a per-file blake3 manifest
//! (`.leindex/fragment_sync_manifest.bin`) so unchanged files are skipped
//! entirely (0 re-embeds), changed files are re-chunked, and only content
//! hashes MISSING from the store are embedded via the caller-provided embed
//! closure (the existing worker's batch path). Store rows + root are then
//! updated under a bumped generation, and hydration refuses a half-synced
//! tree by comparing the manifest generation to the persisted root's.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{FragmentMetadata, FragmentStore};

/// Schema version for the root-hash artifact.
const FRAGMENT_ROOT_SCHEMA_VERSION: u32 = 1;

/// Embedding-schema version folded into the root hash. Bump when the fragment
/// enrichment format changes — the root then differs and a full re-embed is
/// forced (never silently reuse stale embeddings; invariant 3).
pub(crate) const FRAGMENT_EMBEDDING_SCHEMA_VERSION: u32 = 1;

/// Persisted fragment root state (bincode at `.leindex/fragment_root.bin`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FragmentRootState {
    #[serde(default)]
    schema_version: u32,
    /// blake3 hex over sorted (content_hash × embedding-schema-version) pairs.
    pub(crate) root_hash: String,
    /// Monotonic build generation; mid-build generations never serve.
    pub(crate) generation: u64,
    /// Unique embedding rows at the time the root was written.
    pub(crate) fragment_rows: u64,
}

/// Compute the fragment root hash: blake3 over sorted `content_hash:
/// embedding-schema-version` entries. Sorting makes the digest independent of
/// insertion order (a Merkle-root property over the row set).
pub(crate) fn compute_fragment_root_hash(store: &FragmentStore) -> String {
    root_hash_from_entries(store.content_hashes())
}

/// Compute the fragment root hash directly from content-hash ids.
///
/// Used by the snapshot-persist path (`persist_search_snapshot`, Task 5),
/// which holds the collected id → embedding pairs but not a `FragmentStore`.
/// Identical to `compute_fragment_root_hash` — same entries, same digest, so
/// hydration validation (invariant 8) holds regardless of which path wrote it.
pub(crate) fn compute_fragment_root_hash_from_ids(ids: &[String]) -> String {
    root_hash_from_entries(ids.iter().map(String::as_str))
}

/// Shared core: blake3 over sorted `content_hash:embedding-schema-version`
/// entries. Sorting makes the digest independent of insertion order (a
/// Merkle-root property over the row set).
fn root_hash_from_entries<'a>(hashes: impl Iterator<Item = &'a str>) -> String {
    let mut entries: Vec<String> = hashes
        .map(|hash| format!("{hash}:{FRAGMENT_EMBEDDING_SCHEMA_VERSION}"))
        .collect();
    entries.sort();
    let mut hasher = blake3::Hasher::new();
    for entry in entries {
        hasher.update(entry.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

fn fragment_root_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("fragment_root.bin")
}

/// Persist the current root hash + generation for a store.
pub(crate) fn persist_fragment_root(
    project_path: &Path,
    store: &FragmentStore,
    generation: u64,
) -> Result<()> {
    let state = FragmentRootState {
        schema_version: FRAGMENT_ROOT_SCHEMA_VERSION,
        root_hash: compute_fragment_root_hash(store),
        generation,
        fragment_rows: store.len() as u64,
    };
    let path = fragment_root_path(project_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create fragment root directory: {}",
                parent.display()
            )
        })?;
    }
    let payload = bincode::serialize(&state).context("Failed to serialize fragment root")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("Failed to persist fragment root: {}", path.display()))
}

/// Persist the fragment root for a content-hash id list (snapshot-persist
/// path, Task 5). Empty ids remove a stale root file so feature-off leaves no
/// orphan artifact (mirrors the fragment mmap persist behavior).
pub(crate) fn persist_fragment_root_from_ids(
    project_path: &Path,
    ids: &[String],
    generation: u64,
) -> Result<()> {
    let path = fragment_root_path(project_path);
    if ids.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| anyhow::anyhow!("Failed to remove stale fragment root: {e}"))?;
        }
        return Ok(());
    }
    let state = FragmentRootState {
        schema_version: FRAGMENT_ROOT_SCHEMA_VERSION,
        root_hash: compute_fragment_root_hash_from_ids(ids),
        generation,
        fragment_rows: ids.len() as u64,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create fragment root directory: {}",
                parent.display()
            )
        })?;
    }
    let payload = bincode::serialize(&state).context("Failed to serialize fragment root")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("Failed to persist fragment root: {}", path.display()))
}

/// Load the persisted fragment root (None when absent or schema-mismatched).
pub(crate) fn load_fragment_root(storage_path: &Path) -> Result<Option<FragmentRootState>> {
    let path = storage_path.join("fragment_root.bin");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("Failed to read fragment root: {}", path.display()))?;
    let state: FragmentRootState = bincode::deserialize(&bytes)
        .with_context(|| format!("Failed to deserialize fragment root: {}", path.display()))?;
    if state.schema_version != FRAGMENT_ROOT_SCHEMA_VERSION {
        tracing::warn!(
            "Persisted fragment root schema version {} != current {}; discarding",
            state.schema_version,
            FRAGMENT_ROOT_SCHEMA_VERSION
        );
        return Ok(None);
    }
    Ok(Some(state))
}

// ---------------------------------------------------------------------------
// Incremental sync (Task 7): per-file manifest + embed-missing diffing
// ---------------------------------------------------------------------------

/// Schema version for the incremental-sync manifest artifact.
const FRAGMENT_SYNC_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Persisted incremental-sync state: per-file blake3 hashes, the content
/// hashes each file produced, and the monotonic build generation.
///
/// `file_hashes` is what makes re-indexing incremental: a file whose blake3
/// matches the manifest is skipped entirely (0 re-embeds). `file_content_hashes`
/// tracks which store rows each file owns, so a re-chunked file's stale rows
/// are dropped and dedup'd refs are recomputed. Bincode at
/// `.leindex/fragment_sync_manifest.bin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FragmentFileManifest {
    #[serde(default)]
    schema_version: u32,
    /// Monotonic build generation (bumped on every store-changing sync).
    pub(crate) generation: u64,
    /// file path → blake3(raw file bytes) at last sync.
    pub(crate) file_hashes: HashMap<String, String>,
    /// file path → content hashes it produced (stale-row removal on re-chunk).
    #[serde(default)]
    pub(crate) file_content_hashes: HashMap<String, Vec<String>>,
}

impl FragmentFileManifest {
    pub(crate) fn new() -> Self {
        Self {
            schema_version: FRAGMENT_SYNC_MANIFEST_SCHEMA_VERSION,
            generation: 0,
            file_hashes: HashMap::new(),
            file_content_hashes: HashMap::new(),
        }
    }
}

impl Default for FragmentFileManifest {
    fn default() -> Self {
        Self::new()
    }
}

fn fragment_sync_manifest_path(storage_path: &Path) -> PathBuf {
    storage_path.join("fragment_sync_manifest.bin")
}

/// Load the incremental-sync manifest (None when absent or schema-mismatched).
pub(crate) fn load_fragment_sync_manifest(
    storage_path: &Path,
) -> Result<Option<FragmentFileManifest>> {
    let path = fragment_sync_manifest_path(storage_path);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("Failed to read fragment sync manifest: {}", path.display()))?;
    let manifest: FragmentFileManifest = bincode::deserialize(&bytes).with_context(|| {
        format!(
            "Failed to deserialize fragment sync manifest: {}",
            path.display()
        )
    })?;
    if manifest.schema_version != FRAGMENT_SYNC_MANIFEST_SCHEMA_VERSION {
        tracing::warn!(
            "Persisted fragment sync manifest schema version {} != current {}; discarding",
            manifest.schema_version,
            FRAGMENT_SYNC_MANIFEST_SCHEMA_VERSION
        );
        return Ok(None);
    }
    Ok(Some(manifest))
}

/// Persist the incremental-sync manifest to `.leindex/fragment_sync_manifest.bin`.
pub(crate) fn persist_fragment_sync_manifest(
    storage_path: &Path,
    manifest: &FragmentFileManifest,
) -> Result<()> {
    let path = fragment_sync_manifest_path(storage_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create fragment sync manifest directory: {}",
                parent.display()
            )
        })?;
    }
    let payload =
        bincode::serialize(manifest).context("Failed to serialize fragment sync manifest")?;
    std::fs::write(&path, payload).with_context(|| {
        format!(
            "Failed to persist fragment sync manifest: {}",
            path.display()
        )
    })
}

/// One chunked fragment ready for the incremental sync engine.
///
/// `content_hash` is blake3 of `enriched_text` (the exact text that will be
/// embedded — invariant 3: cache key ≡ embedding input). The engine embeds
/// `enriched_text` ONLY when `content_hash` is missing from the store.
#[derive(Debug, Clone)]
pub(crate) struct FragmentCandidate {
    /// blake3(enriched text) — the store row key.
    pub(crate) content_hash: String,
    /// Exact enriched text to embed.
    pub(crate) enriched_text: String,
    /// Store metadata row (owner/file/byte/line ranges).
    pub(crate) meta: FragmentMetadata,
}

/// Result of one incremental sync pass.
#[derive(Debug, Clone, Default)]
pub(crate) struct FragmentSyncSummary {
    pub(crate) files_scanned: usize,
    pub(crate) files_changed: usize,
    pub(crate) fragments_total: usize,
    /// Content-hash rows newly embedded (missing from the store).
    pub(crate) embedded: usize,
    /// Content-hash cache hits (row already in the store, no re-embed).
    pub(crate) reused: usize,
    /// Generation after this sync (unchanged when nothing was dirty).
    pub(crate) generation: u64,
}

/// Embed a batch of missing candidates in batch-256 chunks, inserting rows for
/// successful embeddings only (store row-count ≡ mmap row-count, invariant 8).
/// Returns whether every candidate was embedded; a partial failure leaves the
/// file unmarked in the manifest so a later run retries it.
fn embed_missing_batches(
    missing: &mut [(String, FragmentMetadata)],
    store: &mut FragmentStore,
    embed_fn: &mut dyn FnMut(&[String]) -> Vec<Option<Vec<f32>>>,
    new_embeddings: &mut Vec<(String, Vec<f32>)>,
    summary: &mut FragmentSyncSummary,
    produced: &mut Vec<String>,
) -> (bool, usize) {
    const EMBED_BATCH: usize = 256;
    let mut all_embedded = true;
    let mut inserted = 0;
    for chunk in missing.chunks(EMBED_BATCH) {
        let texts: Vec<String> = chunk.iter().map(|(t, _)| t.clone()).collect();
        let results = embed_fn(&texts);
        for (i, (_, meta)) in chunk.iter().enumerate() {
            match results.get(i) {
                Some(Some(embedding)) if !embedding.is_empty() => {
                    store.insert(meta.clone());
                    summary.embedded += 1;
                    inserted += 1;
                    new_embeddings.push((meta.content_hash.clone(), embedding.clone()));
                }
                _ => {
                    // Embedding unavailable: do NOT insert the row, keeping
                    // store row-count ≡ mmap row-count (invariant 8). Mark the
                    // file incomplete so a later run retries.
                    tracing::warn!(
                        hash = %meta.content_hash,
                        file = %meta.file_path,
                        "Fragment embedding unavailable; row skipped"
                    );
                    produced.retain(|h| h != &meta.content_hash);
                    all_embedded = false;
                }
            }
        }
    }
    (all_embedded, inserted)
}

/// Drop every store row owned by `file_path`, and drop hash entries that end
/// up with no refs (their embedding row is no longer referenced).
fn remove_file_rows(store: &mut FragmentStore, file_path: &str) {
    let mut to_remove: Vec<String> = Vec::new();
    for hash in store
        .content_hashes()
        .map(str::to_string)
        .collect::<Vec<_>>()
    {
        let Some(metas) = store.get(&hash) else {
            continue;
        };
        if metas.iter().any(|m| m.file_path == file_path) {
            let remaining: Vec<FragmentMetadata> = metas
                .iter()
                .filter(|m| m.file_path != file_path)
                .cloned()
                .collect();
            if remaining.is_empty() {
                to_remove.push(hash.clone());
            } else {
                // Rewrite the row with the remaining refs. The store API has no
                // per-ref removal, so rebuild via a fresh row entry.
                store.remove_hash(&hash);
                for meta in remaining {
                    store.insert(meta);
                }
            }
        }
    }
    for hash in to_remove {
        store.remove_hash(&hash);
    }
}

/// Incremental fragment sync engine (Task 7).
///
/// Given the current `(path, blake3)` file set, diff against the persisted
/// manifest, re-chunk ONLY changed files via `chunk_fn`, embed ONLY content
/// hashes missing from the store via `embed_fn` (batch-256 IPC chunks), then
/// update store rows + root under a bumped generation and persist all three
/// artifacts (store, root, manifest). Returns the summary plus the newly
/// embedded `(content_hash, embedding)` rows so the caller can populate the
/// search engine's fragment vector index.
///
/// `project_path` is the project root (`.leindex` artifacts live under it).
/// `chunk_fn(path, bytes)` must return candidates with the enriched text that
/// will be embedded; `embed_fn` maps enriched texts to embeddings (`None`
/// entries are skipped — the row is not inserted, keeping store row-count ≡
/// mmap row-count, invariant 8). A file whose embedding partially fails is NOT
/// marked synced in the manifest, so a later run retries it.
///
/// `force_reembed` recovers from a missing/corrupt fragment embeddings mmap:
/// when the caller detects the store has rows but no recoverable mmap (P2-4,
/// Codex review), it passes `true` so the manifest file-hash skip is bypassed
/// and every content hash is (re)embedded even if it is already in the store —
/// otherwise unchanged files would be skipped, `new_embeddings` would stay
/// empty, an empty fragment index would be installed, and the snapshot path
/// would remove the mmap again, permanently disabling fragment retrieval.
///
/// **Persist ordering is deliberate — store → root → manifest, with the
/// manifest written LAST as the commit marker.** A crash between store and
/// root leaves a complete older root + manifest in place (guard consistent,
/// old mmap serves) and the next sync self-heals: stale rows are removed by
/// `file_path` (never by manifest-listed hashes), so orphan rows from a
/// partial pass are always cleaned. A crash between root and manifest leaves
/// the manifest generation behind the root, which
/// `fragment_layer_generation_is_consistent` rejects — the layer stays off
/// until the next sync repairs it. Either way the fragment layer degrades
/// conservatively; the node-level index is never affected.
/// Outcome of processing one changed file: whether store rows changed.
struct FileSyncOutcome {
    store_modified: bool,
}

/// Process one changed file: drop its stale rows, re-chunk via `chunk_fn`,
/// embed store-missing content hashes via `embed_fn` (batch-256 chunks), and
/// update the manifest. The file hash is committed to the manifest ONLY when
/// every candidate embedded successfully (a partial failure leaves the file
/// unmarked so a later run retries it). Returns whether store rows changed
/// (stale rows dropped or rows added), which drives generation bump + persist.
fn process_changed_file(
    store: &mut FragmentStore,
    manifest: &mut FragmentFileManifest,
    path: &Path,
    file_hash: &str,
    force_reembed: bool,
    chunk_fn: &mut dyn FnMut(&Path, &[u8]) -> Vec<FragmentCandidate>,
    embed_fn: &mut dyn FnMut(&[String]) -> Vec<Option<Vec<f32>>>,
    new_embeddings: &mut Vec<(String, Vec<f32>)>,
    summary: &mut FragmentSyncSummary,
) -> FileSyncOutcome {
    let path_str = path.display().to_string();
    let mut store_modified = false;

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "Failed to read source file during fragment sync; skipping"
            );
            return FileSyncOutcome { store_modified };
        }
    };

    // Drop this file's stale rows before inserting the fresh candidates.
    if manifest.file_content_hashes.remove(&path_str).is_some() {
        remove_file_rows(store, &path_str);
        store_modified = true;
    }

    let candidates = chunk_fn(path, &bytes);
    summary.fragments_total += candidates.len();
    let mut missing: Vec<(String, FragmentMetadata)> = Vec::new(); // (enriched_text, meta)
    let mut produced: Vec<String> = Vec::new();

    for cand in candidates {
        // A store cache hit is dedup'd (no re-embed) UNLESS forcing a
        // re-embed — a recovered mmap needs every row, not just new ones.
        if !force_reembed && store.get(&cand.content_hash).is_some() {
            // Content-hash cache hit: dedup'd, no re-embed. Add the ref.
            summary.reused += 1;
            store.insert(cand.meta.clone());
            store_modified = true;
            produced.push(cand.content_hash);
        } else {
            // Recovery-only note: when `force_reembed` is set (lost fragment
            // mmap), every candidate lands here even if its hash is already in
            // the store — identical cross-file fragments are re-embedded once
            // per file and accrue duplicate store refs. Harmless (root,
            // fragment_refs, and the mmap all key by unique hash and
            // collapse) and bounded to the one-time recovery pass; do not
            // "optimize" this back into the cache-hit path.
            missing.push((cand.enriched_text, cand.meta));
            produced.push(cand.content_hash);
        }
    }

    // Embed the missing hashes, in batch-256 chunks (existing IPC batch).
    let (all_embedded, inserted) = embed_missing_batches(
        &mut missing,
        store,
        embed_fn,
        new_embeddings,
        summary,
        &mut produced,
    );
    // Rows inserted by the embed path change the store too — without this the
    // FIRST sync (all-new fragments) would never persist store/root or bump
    // the generation (latent pre-refactor bug: `store_dirty` was only set on
    // the cache-hit and stale-row-removal paths).
    store_modified |= inserted > 0;

    manifest
        .file_content_hashes
        .insert(path_str.clone(), produced);
    if all_embedded {
        manifest.file_hashes.insert(path_str, file_hash.to_string());
    }
    FileSyncOutcome { store_modified }
}

pub(crate) fn incremental_sync_fragments(
    project_path: &Path,
    store: &mut FragmentStore,
    files: &[(PathBuf, String)],
    chunk_fn: &mut dyn FnMut(&Path, &[u8]) -> Vec<FragmentCandidate>,
    embed_fn: &mut dyn FnMut(&[String]) -> Vec<Option<Vec<f32>>>,
    force_reembed: bool,
) -> Result<(FragmentSyncSummary, Vec<(String, Vec<f32>)>)> {
    let storage_path = project_path.join(".leindex");
    let mut manifest = load_fragment_sync_manifest(&storage_path)?.unwrap_or_default();
    let mut summary = FragmentSyncSummary {
        files_scanned: files.len(),
        ..Default::default()
    };
    let current_paths: HashSet<String> =
        files.iter().map(|(p, _)| p.display().to_string()).collect();
    let mut new_embeddings: Vec<(String, Vec<f32>)> = Vec::new();
    let mut store_dirty = false;
    let mut manifest_dirty = false;

    // Removed files (in manifest, not in the current set): drop their rows.
    let removed_paths: Vec<String> = manifest
        .file_hashes
        .keys()
        .filter(|p| !current_paths.contains(*p))
        .cloned()
        .collect();
    for path in &removed_paths {
        if manifest.file_content_hashes.remove(path).is_some() {
            remove_file_rows(store, path);
            store_dirty = true;
        }
        manifest.file_hashes.remove(path);
        manifest_dirty = true;
    }

    for (path, file_hash) in files {
        let path_str = path.display().to_string();
        // Unchanged files are skipped entirely (0 re-embeds) UNLESS the caller
        // forces a re-embed to recover from a lost fragment mmap (P2-4).
        if !force_reembed && manifest.file_hashes.get(&path_str) == Some(file_hash) {
            continue;
        }
        summary.files_changed += 1;
        manifest_dirty = true;
        let outcome = process_changed_file(
            store,
            &mut manifest,
            path,
            file_hash,
            force_reembed,
            chunk_fn,
            embed_fn,
            &mut new_embeddings,
            &mut summary,
        );
        store_dirty |= outcome.store_modified;
    }

    if store_dirty {
        manifest.generation += 1;
        store.persist_to_storage(project_path)?;
        persist_fragment_root(project_path, store, manifest.generation)?;
        summary.generation = manifest.generation;
    } else {
        summary.generation = manifest.generation;
    }
    if manifest_dirty {
        persist_fragment_sync_manifest(&storage_path, &manifest)?;
    }

    Ok((summary, new_embeddings))
}

/// Validate that a persisted fragment layer is NOT half-synced: the sync
/// manifest generation must match the persisted root generation.
///
/// The manifest is written together with the store + root under one bumped
/// generation, so a mid-build crash leaves either an older root (generation
/// mismatch → stale → caller serves the last complete root, i.e. disables the
/// layer) or no manifest at all (legacy Task 5/6 artifacts → accept).
pub(crate) fn fragment_layer_generation_is_consistent(
    storage_path: &Path,
    root: &FragmentRootState,
) -> bool {
    match load_fragment_sync_manifest(storage_path) {
        Ok(Some(manifest)) => manifest.generation == root.generation,
        // No manifest: pre-incremental-sync artifacts (Tasks 5/6) — accept.
        Ok(None) => true,
        Err(_) => false,
    }
}
