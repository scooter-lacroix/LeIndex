//! Search snapshot and staged-retrieval configuration/metrics.

use super::*;

/// Persisted metadata needed to hydrate SearchEngine without re-running the
/// source-content indexing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SearchSnapshot {
    pub(crate) version: u32,
    pub(crate) pdg_nodes: usize,
    pub(crate) pdg_edges: usize,
    pub(crate) pdg_fingerprint: String,
    pub(crate) indexed_nodes: usize,
    pub(crate) nodes: Vec<SearchSnapshotNode>,
    /// Fragment layer root hash (filled by the cli-side persist path;
    /// `None` for legacy/feature-off snapshots).
    #[serde(default)]
    pub(crate) fragment_root_hash: Option<String>,
    /// Unique fragment embedding rows recorded at persist time. Serde-defaults
    /// to 0 for legacy snapshots (fragment layer off).
    #[serde(default)]
    pub(crate) fragment_rows: u32,
}

/// Per-node metadata for fast search-index hydration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SearchSnapshotNode {
    pub(crate) node_id: String,
    pub(crate) file_path: String,
    pub(crate) symbol_name: String,
    pub(crate) language: String,
    pub(crate) byte_range: (usize, usize),
    pub(crate) complexity: u32,
    pub(crate) signature: Option<String>,
    pub(crate) tokens: Vec<String>,
}

// A+ Search cache budget constants (Section 8.1)
/// Maximum entries in the search cache.
pub const SEARCH_CACHE_MAX_ENTRIES: usize = 256;
/// Maximum total bytes for the search cache.
pub const SEARCH_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

// ============================================================================
// STAGED RETRIEVAL (Plan 2 — VAL-BPHASE-044, VAL-BPHASE-045)
// ============================================================================

/// Configuration for staged retrieval: coarse candidate generation followed
/// by exact rerank.
///
/// Staged retrieval reduces exact-stage work by first narrowing the candidate
/// set with a cheap coarse pass (TF-IDF vector similarity only), then applying
/// the full hybrid scoring (text + TF-IDF + structural) only to the reduced
/// candidate set.
///
/// **Important**: This is a coarse-prefilter-plus-exact-rerank design. It does
/// **not** replace the approved INT8/default quality-gated path with
/// binary-quantization-first search. The existing `search()` method remains
/// the authoritative default; staged retrieval is an opt-in optimization.
#[derive(Debug, Clone)]
pub struct StagedRetrievalConfig {
    /// Whether staged retrieval is enabled.
    ///
    /// When `false`, `search_staged` falls back to the standard `search` path.
    /// When `true`, the coarse-then-exact pipeline is used.
    pub enabled: bool,

    /// Multiplier applied to `top_k` to determine the coarse candidate set
    /// size. For example, with `top_k = 10` and `coarse_multiplier = 5`,
    /// the coarse phase retrieves `50` candidates, then the exact rerank
    /// narrows to the best `10`.
    ///
    /// Must be >= 1. Higher values improve recall at the cost of more exact
    /// scoring work.
    pub coarse_multiplier: usize,
}

impl Default for StagedRetrievalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            coarse_multiplier: 5,
        }
    }
}

impl StagedRetrievalConfig {
    /// Create a new config with staged retrieval enabled and the given
    /// coarse multiplier.
    pub fn enabled_with_multiplier(coarse_multiplier: usize) -> Self {
        Self {
            enabled: true,
            coarse_multiplier: coarse_multiplier.max(1),
        }
    }

    /// Create a disabled config (staged retrieval off).
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Metrics reported by a staged retrieval pass.
///
/// Allows tests and callers to observe that the staged path actually reduced
/// exact-stage work (VAL-BPHASE-045).
#[derive(Debug, Clone, Default)]
pub struct StagedRetrievalMetrics {
    /// Number of candidates produced by the coarse phase.
    pub coarse_candidates: usize,
    /// Number of candidates scored by the exact rerank phase.
    pub exact_scored: usize,
    /// Final number of results returned after rerank.
    pub results_returned: usize,
    /// Whether staged retrieval was actually used (vs. fallback to standard).
    pub staged_used: bool,
}
