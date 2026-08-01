//! Fragment sync: root-hash computation and generation-state persistence
//! (fragment-embeddings 1.11.0 Task 3; incremental diffing lands in Task 7).
//!
//! The fragment root hash is a blake3 digest over the sorted set of
//! `content_hash × embedding-schema-version` pairs, so any change to a
//! fragment's embedded text (content hash) — or to the enrichment format
//! (schema version) — changes the root. Hydration rejects a fragment mmap
//! whose row count or root hash mismatches the persisted root (invariant 8),
//! mirroring the `restore_from_search_snapshot` discipline. Persisted at
//! `.leindex/fragment_root.bin` with a monotonic generation counter.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::FragmentStore;

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
    let mut entries: Vec<String> = store
        .content_hashes()
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
