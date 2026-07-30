//! Node/query/result types and search-engine data types.

use super::*;

/// bridge: the bridge prefers `tfidf_embedding` when present and non-empty,
/// otherwise promotes the legacy `embedding` value, otherwise defaults to empty.
/// Serialization always emits only the new layout (no `embedding` field).
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Unique node ID
    pub node_id: String,

    /// File path
    pub file_path: String,

    /// Symbol name
    pub symbol_name: String,

    /// Programming language
    pub language: String,

    /// Source content
    pub content: String,

    /// Byte range in source
    pub byte_range: (usize, usize),

    /// TF-IDF embedding (always present, 768-dim, for hybrid search)
    pub tfidf_embedding: Vec<f32>,

    /// Neural/remote embedding for the configured hybrid scorer. This is
    /// populated during the active neural phase and is absent only when the
    /// provider is disabled or reaches an explicit terminal failure.
    pub neural_embedding: Option<Vec<f32>>,

    /// Complexity score (0-100+, higher = more complex)
    pub complexity: u32,

    /// Cached signature extracted from content (for search results)
    /// This is extracted before content is cleared during T13 optimization
    pub signature: Option<String>,

    /// Pre-tokenized search tokens (lowercased, filtered by length >= 2).
    ///
    /// When `Some`, these tokens are used directly for the inverted index
    /// instead of re-tokenizing from `content`. This enables callers that
    /// already have tokenized content (e.g., `index_builder`) to skip the
    /// redundant split+lowercase pass.
    ///
    /// Backward-compatible: `None` falls back to `content.split()` tokenization.
    pub pre_tokenized: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Compatibility bridge: deserialize both old and new payload shapes
// ---------------------------------------------------------------------------

/// Intermediate representation used during deserialization to accept both the
/// legacy shape (`embedding: Option<Vec<f32>>`) and the new shape
/// (`tfidf_embedding: Vec<f32>`).
#[derive(Deserialize)]
struct NodeInfoRepr {
    node_id: String,
    file_path: String,
    symbol_name: String,
    language: String,
    content: String,
    byte_range: (usize, usize),

    #[serde(default)]
    tfidf_embedding: Vec<f32>,

    #[serde(default)]
    neural_embedding: Option<Vec<f32>>,

    /// Legacy field — accepted from old payloads but never written back out.
    #[serde(default, alias = "embedding")]
    legacy_embedding: Option<Vec<f32>>,

    #[serde(default)]
    complexity: u32,

    #[serde(default)]
    signature: Option<String>,

    #[serde(default)]
    pre_tokenized: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for NodeInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let repr = NodeInfoRepr::deserialize(deserializer)?;

        // Resolution rule (per spec §5.5):
        //   1. Prefer tfidf_embedding if present and non-empty.
        //   2. Otherwise promote legacy embedding value if present and non-empty.
        //   3. Otherwise default to empty.
        let tfidf_embedding = if !repr.tfidf_embedding.is_empty() {
            repr.tfidf_embedding
        } else if let Some(legacy) = repr.legacy_embedding {
            if !legacy.is_empty() {
                legacy
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        Ok(Self {
            node_id: repr.node_id,
            file_path: repr.file_path,
            symbol_name: repr.symbol_name,
            language: repr.language,
            content: repr.content,
            byte_range: repr.byte_range,
            tfidf_embedding,
            neural_embedding: repr.neural_embedding,
            complexity: repr.complexity,
            signature: repr.signature,
            pre_tokenized: repr.pre_tokenized,
        })
    }
}

impl Serialize for NodeInfo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Always serialize the new layout — never write the legacy `embedding` field.
        #[derive(Serialize)]
        struct NodeInfoNew<'a> {
            node_id: &'a str,
            file_path: &'a str,
            symbol_name: &'a str,
            language: &'a str,
            content: &'a str,
            byte_range: (usize, usize),
            tfidf_embedding: &'a [f32],
            neural_embedding: &'a Option<Vec<f32>>,
            complexity: u32,
            signature: &'a Option<String>,
            pre_tokenized: &'a Option<Vec<String>>,
        }

        NodeInfoNew {
            node_id: &self.node_id,
            file_path: &self.file_path,
            symbol_name: &self.symbol_name,
            language: &self.language,
            content: &self.content,
            byte_range: self.byte_range,
            tfidf_embedding: &self.tfidf_embedding,
            neural_embedding: &self.neural_embedding,
            complexity: self.complexity,
            signature: &self.signature,
            pre_tokenized: &self.pre_tokenized,
        }
        .serialize(serializer)
    }
}

/// Pre-computed query data for optimized text scoring
///
/// This struct holds data that is pre-computed once per search to avoid
/// repeated allocations in the hot path. When searching N nodes, this reduces
/// allocations from O(N) to O(1).
pub(super) struct TextQueryPreprocessed {
    /// Lowercase query for case-insensitive matching
    pub(super) query_lower: String,
    /// Query tokens for overlap calculation
    pub(super) query_tokens: HashSet<String>,
}

impl TextQueryPreprocessed {
    /// Create pre-computed query data
    pub(super) fn from_query(query: &str) -> Self {
        let query_lower = query.to_ascii_lowercase();
        // Tokenize using the same logic as the content indexing
        let query_tokens: HashSet<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        Self {
            query_lower,
            query_tokens,
        }
    }
}

// ============================================================================
// SEARCH QUERY
// ============================================================================

/// Search query
///
/// This represents a search request with all parameters needed to execute
/// a search operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Query text
    pub query: String,

    /// Maximum results to return (validated by QueryParser)
    pub top_k: usize,

    /// Token budget for context expansion (validated by QueryParser)
    pub token_budget: Option<usize>,

    /// Whether to use semantic search
    pub semantic: bool,

    /// Whether to expand context using graph traversal
    pub expand_context: bool,

    /// Optional query embedding for semantic search (TF-IDF)
    pub query_embedding: Option<Vec<f32>>,

    /// Configured neural query embedding for semantic/deep search.
    ///
    /// Populated after the configured provider reaches `Ready`. When `None`,
    /// the result metadata records the terminal/disabled state and the
    /// mandatory TF-IDF score remains available.
    pub query_neural_embedding: Option<Vec<f32>>,

    /// Minimum relevance threshold (0.0-1.0)
    pub threshold: Option<f32>,

    /// Query type for adaptive ranking
    pub query_type: Option<crate::search::ranking::QueryType>,
}

// ============================================================================
// SEARCH RESULT
// ============================================================================

/// Search result
///
/// This represents a single search result with all relevant metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Result rank (1-based)
    pub rank: usize,

    /// Node ID
    pub node_id: String,

    /// File path
    pub file_path: String,

    /// Symbol name
    pub symbol_name: String,

    /// Symbol type: function | method | class | variable | module
    ///
    /// Populated by `LeIndex::search()` from PDG node type.
    /// `None` when the node is not in the PDG (e.g., external refs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_type: Option<String>,

    /// First line of the symbol's source (declaration / signature).
    ///
    /// Extracted from `node.content` — the second line after the
    /// `// name in path` header comment, trimmed of leading whitespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Cyclomatic complexity score of the symbol.
    pub complexity: u32,

    /// Number of call-sites that invoke this symbol (direct callers in PDG).
    ///
    /// Populated by `LeIndex::search()`. `None` if PDG is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_count: Option<usize>,

    /// Number of symbols this symbol depends on (outgoing PDG edges).
    ///
    /// Populated by `LeIndex::search()`. `None` if PDG is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_count: Option<usize>,

    /// Programming language
    pub language: String,

    /// Relevance score
    pub score: Score,

    /// Expanded context (if requested)
    pub context: Option<String>,

    /// Byte range in source
    pub byte_range: (usize, usize),

    /// 1-based line number of the symbol's definition in the source file.
    ///
    /// Populated by `LeIndex::search()` from the PDG node's byte_range.
    /// `None` when the line number cannot be determined (e.g., no PDG).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_number: Option<usize>,
}

// ============================================================================
// SEARCH ENGINE
// ============================================================================

// ---------------------------------------------------------------------------
// B-phase compact resident metadata (Plan 2)
// ---------------------------------------------------------------------------

/// Compact token-to-rows index using integer-backed addressing.
///
/// Maps each token to a set of row indices (`u32`) instead of string node IDs.
/// This is the compressed form of the inverted index for resident search state.
#[derive(Debug, Clone)]
pub struct CompactTokenIndex {
    /// Token → set of row indices.
    pub(super) token_rows: HashMap<String, HashSet<u32>>,
}

impl CompactTokenIndex {
    /// Return the set of row indices that contain the given token.
    ///
    /// Returns an empty set if the token is not in the index.
    pub fn nodes_for_token(&self, token: &str) -> &HashSet<u32> {
        static EMPTY: std::sync::OnceLock<HashSet<u32>> = std::sync::OnceLock::new();
        self.token_rows
            .get(token)
            .unwrap_or_else(|| EMPTY.get_or_init(HashSet::new))
    }

    /// Return the number of distinct tokens in the index.
    pub fn token_count(&self) -> usize {
        self.token_rows.len()
    }
}

/// Compact, row-oriented snapshot of resident search metadata.
///
/// Uses `u32` row indices instead of string-heavy node-ID maps. This is the
/// B-phase compressed resident state that reduces memory overhead while
/// preserving stable lookup semantics (VAL-BPHASE-041).
#[derive(Debug, Clone)]
pub struct CompactNodeMetadata {
    /// Node ID → row index mapping (compact u32 addressing).
    pub(super) row_map: Vec<(String, u32)>,
    /// Complexity values indexed by row (compact u32 array).
    pub(super) complexity_by_row: Vec<u32>,
    /// Token index using row-based addressing.
    pub(super) token_index: CompactTokenIndex,
}

impl CompactNodeMetadata {
    /// Look up the row index for a given node ID.
    ///
    /// Returns `None` if the node is not in the compact metadata.
    pub fn row_index(&self, node_id: &str) -> Option<u32> {
        self.row_map
            .iter()
            .find(|(id, _)| id == node_id)
            .map(|(_, row)| *row)
    }

    /// Look up the complexity for a given row index.
    ///
    /// Returns `None` if the row is out of range.
    pub fn complexity_by_row(&self, row: u32) -> Option<u32> {
        self.complexity_by_row.get(row as usize).copied()
    }

    /// Return the compact token index.
    pub fn token_index(&self) -> &CompactTokenIndex {
        &self.token_index
    }

    /// Return the number of nodes in the compact metadata.
    pub fn node_count(&self) -> usize {
        self.row_map.len()
    }
}

/// engine.incremental_reindex(delta);
/// ```
#[derive(Debug, Default)]
pub struct TextIndexDelta {
    /// Node IDs to remove from the index.
    pub removed_node_ids: Vec<String>,
    /// New or updated nodes to add to the index.
    /// Nodes whose `node_id` already exists will be replaced in-place.
    pub updated_nodes: Vec<NodeInfo>,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Entry type for semantic search results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryType {
    /// Function entry point
    Function,
    /// Method entry point
    Method,
    /// Class/struct entry point
    Class,
    /// Module-level entry point
    Module,
}

/// Semantic entry for entry point detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEntry {
    /// Node ID
    pub node_id: String,
    /// Relevance score
    pub relevance: f32,
    /// Entry type
    pub entry_type: EntryType,
}

/// Search errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Query execution failed
    #[error("Query failed: {0}")]
    QueryFailed(String),

    /// Index is empty
    #[error("Index is empty")]
    EmptyIndex,

    /// Dimension mismatch
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },
}
