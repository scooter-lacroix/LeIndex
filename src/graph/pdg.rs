// Program Dependence Graph — Rewrite
//
// Key changes from original:
//   - `EdgeType::Containment` added (Class→Method, Module→Function structural edges)
//   - `TraversalConfig` drives all impact/traversal methods — no more unbounded variants
//   - Embeddings externalized to `EmbeddingStore` (separate HashMap<NodeId, Vec<f32>>)
//   - `find_by_name_in_file` O(n) fallbacks eliminated via normalized secondary index
//   - `add_edge` returns `EdgeId` directly (was misleadingly Option<EdgeId>)
//   - All public traversal methods take `TraversalConfig` — callers must be explicit

use crate::fast_hash::{FastMap as HashMap, FastSet as HashSet};
use bincode::Options;
use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};

use crate::graph::trigram::TrigramIndex;

mod lookup;

mod edges_traversal_serde;

/// A unique identifier for a node in the Program Dependence Graph.
///
/// This is a type alias for `petgraph::stable_graph::NodeIndex`, which provides
/// a compact, copyable handle to a specific node in the graph. NodeIds remain
/// stable even as the graph is modified (nodes are marked as removed but indices
/// are not reused).
pub type NodeId = petgraph::stable_graph::NodeIndex;

/// A unique identifier for an edge in the Program Dependence Graph.
///
/// This is a type alias for `petgraph::stable_graph::EdgeIndex`, which provides
/// a compact, copyable handle to a specific edge in the graph. Like NodeIds,
/// EdgeIds remain stable during graph modifications.
pub type EdgeId = petgraph::stable_graph::EdgeIndex;

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

/// Canonical `file_path` for external placeholder nodes. Real file paths
/// would tie a shared placeholder to whichever file's extraction pass
/// created it, letting a per-file `remove_file`/`delete_file_data` delete a
/// node other files' edges still point at. The graph layer, the storage
/// loader, and both indexing build routes canonicalize to this value so
/// externals are graph-level vocabulary, never file content.
pub const EXTERNAL_NODE_FILE_PATH: &str = "<external>";

/// A node in the Program Dependence Graph representing a code entity.
///
/// Each node represents a distinct code element such as a function, class,
/// method, variable, or module. Nodes contain metadata about the entity
/// including its location, type, complexity, and language.
///
/// **Note on embeddings:** Embeddings have been externalized to `EmbeddingStore`
/// to reduce memory usage. Previously, storing ~6KB per node for 50k nodes
/// would consume ~300MB. Now embeddings are stored separately and loaded
/// on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// Fully qualified unique identifier for this node.
    ///
    /// Format varies by language but typically includes file path and symbol name,
    /// e.g., "src/main.rs:my_module::my_function".
    pub id: String,

    /// The type of code entity this node represents.
    pub node_type: NodeType,

    /// The human-readable name of the symbol (function name, class name, etc.).
    pub name: String,

    /// Absolute path to the file containing this node.
    ///
    /// Uses `Arc<str>` for string interning: nodes in the same file share
    /// the same allocation, avoiding per-node path duplication.
    pub file_path: Arc<str>,

    /// Byte range (start, end) within the source file where this node is defined.
    pub byte_range: (usize, usize),

    /// Cyclomatic complexity of the code entity (for functions/methods).
    ///
    /// For non-functional types (classes, variables), this is typically 0.
    pub complexity: u32,

    /// The programming language of the source code (e.g., "rust", "python", "javascript").
    pub language: String,
    // NOTE: embeddings removed from Node. Use EmbeddingStore instead.
    // Keeping this field as Option<()> would break existing bincode; instead
    // the serialization shim below handles backward compat via a skip field.
}

impl Node {
    /// Construct a synthetic per-file summary node. (conceptual-recall fix)
    ///
    /// `byte_range=(0,0)` (no source snippet); `enriched_node_content` builds
    /// the embedded text from the file's leading doc + same-file item names.
    pub fn new_file_summary(file_path: &str, language: &str) -> Self {
        let stem = std::path::Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        Self {
            id: format!("{}::file_summary", file_path),
            node_type: NodeType::FileSummary,
            name: stem,
            file_path: std::sync::Arc::from(file_path),
            byte_range: (0, 0),
            complexity: 0,
            language: language.to_string(),
        }
    }
}

/// The type of code entity a node represents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeType {
    /// A standalone function (not a method).
    Function,

    /// A class or struct definition.
    Class,

    /// A method belonging to a class or struct.
    Method,

    /// A variable or constant declaration.
    Variable,

    /// A module, namespace, or package.
    Module,

    /// Imported/referenced symbol not defined in this project
    External,

    /// A documentation heading section (markdown/rst/adoc/txt docs tier).
    DocSection,

    /// Synthetic per-file summary node (conceptual-recall fix). Embeds the
    /// file's leading doc comment + the names of its top-level items as a
    /// single retrievable unit, so a conceptual NL query can match a file by
    /// its *purpose* even when no individual function does. `byte_range=(0,0)`.
    FileSummary,
}

/// Edge types — now includes Containment for structural (non-semantic) relationships.
///
/// Filtering guidance for callers:
///   - Call + DataDependency + Inheritance + TypeOf = semantic graph (use for impact analysis)
///   - Containment = structural graph (use for hierarchy display, not reachability)
///   - Import = module-level dependency graph
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EdgeType {
    /// Direct function/method call
    Call,
    /// Data flows from one node to another (return→param signal)
    DataDependency,
    /// Inheritance / interface implementation
    Inheritance,
    /// Module import dependency
    Import,
    /// Structural containment: Class contains Method, Module contains Function.
    /// NOT a semantic dependency. Exclude from impact traversal by default.
    Containment,
    /// A state transition such as install result → verification → registry write.
    StateTransition,
    /// An argument passed to an external command (`argv`).
    CommandArgument,
    /// An environment variable passed to an external command.
    Environment,
    /// Bytes or a value passed to command standard input.
    Stdin,
    /// A precise type-of relationship confirmed by SCIP.
    ///
    /// Kept at the end of the enum to preserve bincode discriminants for the
    /// pre-TypeOf edge variants.
    TypeOf,
}

/// An edge in the Program Dependence Graph representing a relationship between nodes.
///
/// Edges connect nodes with semantic meaning (Call, DataDependency, Inheritance, Import)
/// or structural meaning (Containment). The edge type determines how the edge should
/// be interpreted and used in analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// The type of relationship this edge represents.
    pub edge_type: EdgeType,

    /// Additional metadata about this edge including confidence scores,
    /// call counts, and variable names for data flow tracking.
    pub metadata: EdgeMetadata,
}

/// Metadata associated with a PDG edge.
///
/// Contains optional information that enriches the edge with additional
/// context. Not all fields are populated for all edge types:
///
/// - `call_count`: Populated for Call edges
/// - `variable_name`: Populated for DataDependency edges
/// - `confidence`: Populated for inferred edges (Inheritance, DataDependency signals)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMetadata {
    /// Number of times this call relationship was observed in the codebase.
    ///
    /// Only meaningful for Call edges. Higher counts indicate hot paths.
    pub call_count: Option<usize>,

    /// Name of the variable through which data flows.
    ///
    /// Only meaningful for DataDependency edges. Helps trace specific
    /// data flow paths through the codebase.
    pub variable_name: Option<String>,

    /// Confidence score [0.0, 1.0] for inferred edges.
    ///
    /// Used for inheritance relationships and data flow signals where the
    /// relationship is inferred rather than explicitly declared. Higher
    /// values indicate stronger evidence for the relationship.
    pub confidence: Option<f32>,

    /// Flow channel (`argument`, `env`, `stdin`, etc.) when the edge was
    /// extracted from a source-level flow fact.
    #[serde(default)]
    pub channel: Option<String>,

    /// Argument ordinal for call/data-flow edges.
    #[serde(default)]
    pub position: Option<usize>,
}

impl EdgeMetadata {
    /// Creates an empty EdgeMetadata with all fields set to None.
    ///
    /// Use this for edges that don't require any additional metadata,
    /// such as simple containment relationships.
    pub fn empty() -> Self {
        Self {
            call_count: None,
            variable_name: None,
            confidence: None,
            channel: None,
            position: None,
        }
    }

    /// Creates EdgeMetadata with a confidence score for inferred edges.
    ///
    /// # Arguments
    ///
    /// * `confidence` - A value in the range [0.0, 1.0] representing the
    ///   confidence in this inferred relationship.
    ///
    /// # Examples
    ///
    /// Used for inheritance edges (0.45-0.90) and data flow signals (0.45-0.85).
    pub fn with_confidence(confidence: f32) -> Self {
        Self {
            call_count: None,
            variable_name: None,
            confidence: Some(confidence),
            channel: None,
            position: None,
        }
    }

    /// Creates EdgeMetadata with a variable name for data flow tracking.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the variable that carries data between nodes.
    ///
    /// # Examples
    ///
    /// Used for data dependency edges to identify which variable flows
    /// from a producer function to a consumer function.
    pub fn with_variable(name: String) -> Self {
        Self {
            call_count: None,
            variable_name: Some(name),
            confidence: None,
            channel: None,
            position: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Traversal configuration
// ---------------------------------------------------------------------------

/// Controls all graph traversal operations.
///
/// Replaces the proliferation of `_bounded` / `_filtered` variants.
/// Callers must construct this explicitly — no hidden defaults that permit
/// unbounded traversal.
///
/// # Recommended defaults by use case
///
/// | Use case                    | max_depth | max_nodes | allowed_edge_types              |
/// |-----------------------------|-----------|-----------|----------------------------------|
/// | LLM context window (tight)  | 3         | 50        | Call, DataDependency             |
/// | LLM context window (broad)  | 5         | 150       | Call, DataDependency, Inheritance, TypeOf|
/// | Impact analysis (full)      | None      | 500       | Call, DataDependency, Inheritance, TypeOf|
/// | Module dependency map       | 10        | 1000      | Import                           |
/// | Class hierarchy display     | 8         | 200       | Inheritance, Containment         |
#[derive(Debug, Clone)]
pub struct TraversalConfig {
    /// Maximum hop depth from the start node. `None` = unlimited (use carefully).
    pub max_depth: Option<usize>,
    /// Hard ceiling on number of nodes collected. Prevents runaway traversal.
    /// Strongly recommended: always set this. `None` = unlimited.
    pub max_nodes: Option<usize>,
    /// Only traverse edges of these types. `None` = all edge types.
    /// Uses a static slice to eliminate heap allocation during traversal.
    pub allowed_edge_types: Option<&'static [EdgeType]>,
    /// Do not collect nodes of these types (but still traverse through them).
    pub excluded_node_types: Option<Vec<NodeType>>,
    /// Skip collecting nodes with complexity below this threshold.
    pub min_complexity: Option<u32>,
    /// Minimum confidence for inferred edges (DataDependency, Inheritance).
    /// Edges without a confidence value always pass. Default: 0.0 (all pass).
    pub min_edge_confidence: f32,
}

impl TraversalConfig {
    /// Tight config for LLM context construction — aggressive limits.
    pub fn for_llm_context() -> Self {
        Self {
            max_depth: Some(3),
            max_nodes: Some(50),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::StateTransition,
                EdgeType::CommandArgument,
                EdgeType::Environment,
                EdgeType::Stdin,
            ]),
            excluded_node_types: Some(vec![NodeType::Module]),
            min_complexity: None,
            min_edge_confidence: 0.5,
        }
    }

    /// Broad semantic analysis — includes inheritance and precise type edges,
    /// with moderate limits.
    pub fn for_semantic_analysis() -> Self {
        Self {
            max_depth: Some(5),
            max_nodes: Some(150),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
                EdgeType::TypeOf,
                EdgeType::StateTransition,
                EdgeType::CommandArgument,
                EdgeType::Environment,
                EdgeType::Stdin,
            ]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.4,
        }
    }

    /// Full impact analysis — all semantic edges, including precise type
    /// relationships, with a hard node cap.
    pub fn for_impact_analysis() -> Self {
        Self {
            max_depth: None,
            max_nodes: Some(500),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
                EdgeType::TypeOf,
                EdgeType::StateTransition,
                EdgeType::CommandArgument,
                EdgeType::Environment,
                EdgeType::Stdin,
            ]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.0,
        }
    }

    /// Module dependency graph only.
    pub fn for_import_graph() -> Self {
        Self {
            max_depth: Some(10),
            max_nodes: Some(1000),
            allowed_edge_types: Some(&[EdgeType::Import]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.0,
        }
    }

    fn edge_allowed(&self, edge: &Edge) -> bool {
        let type_ok = self
            .allowed_edge_types
            .as_ref()
            .map(|types| types.contains(&edge.edge_type))
            .unwrap_or(true);

        let confidence_ok = edge
            .metadata
            .confidence
            .map(|c| c >= self.min_edge_confidence)
            .unwrap_or(true);

        type_ok && confidence_ok
    }

    fn node_should_collect(&self, node: &Node) -> bool {
        let type_ok = self
            .excluded_node_types
            .as_ref()
            .map(|excluded| !excluded.contains(&node.node_type))
            .unwrap_or(true);

        let complexity_ok = self
            .min_complexity
            .map(|min| node.complexity >= min)
            .unwrap_or(true);

        type_ok && complexity_ok
    }
}

// ---------------------------------------------------------------------------
// Embedding store (externalized from Node)
// ---------------------------------------------------------------------------

/// Stores node embeddings separately from the graph structure.
///
/// Rationale: At 50k nodes with 1536-dim embeddings, inline storage adds ~300MB
/// to the graph struct. This store is optional — the graph operates fully without it.
#[derive(Debug, Default, Clone)]
pub struct EmbeddingStore {
    pub(crate) embeddings: HashMap<String, Vec<f32>>, // keyed by node.id (stable across serialization)
}

impl EmbeddingStore {
    /// Creates a new, empty EmbeddingStore.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or updates an embedding for a node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The unique identifier of the node (must match `Node.id`)
    /// * `embedding` - The vector embedding (typically 1536 dimensions for OpenAI models)
    pub fn insert(&mut self, node_id: &str, embedding: Vec<f32>) {
        self.embeddings.insert(node_id.to_string(), embedding);
    }

    /// Retrieves the embedding for a node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The unique identifier of the node
    ///
    /// # Returns
    ///
    /// An optional reference to the embedding vector if it exists.
    pub fn get(&self, node_id: &str) -> Option<&Vec<f32>> {
        self.embeddings.get(node_id)
    }

    /// Removes the embedding for a node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The unique identifier of the node to remove
    pub fn remove(&mut self, node_id: &str) {
        self.embeddings.remove(node_id);
    }

    /// Returns the number of embeddings stored.
    pub fn len(&self) -> usize {
        self.embeddings.len()
    }

    /// Returns true if no embeddings are stored.
    pub fn is_empty(&self) -> bool {
        self.embeddings.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Serialization shim
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableNode {
    index: u32,
    node: Node,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializableEdge {
    source: u32,
    target: u32,
    edge: Edge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDG {
    nodes: Vec<SerializableNode>,
    edges: Vec<SerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_lower_index: HashMap<String, Vec<u32>>,
    /// Embeddings stored separately from nodes for memory efficiency.
    /// Keyed by node.id string, value is the embedding vector.
    #[serde(default)]
    embeddings: HashMap<String, Vec<f32>>,
    /// Stable node identifiers confirmed by SCIP precision ingest.
    ///
    /// This is the final field so older bincode payloads can be retried with a
    /// legacy schema when they do not contain precision markers.
    #[serde(default)]
    precision_symbols: HashSet<String>,
}

/// The bincode schema written before precision markers were added.
///
/// `serde(default)` does not make a missing trailing field backward-compatible
/// for bincode, so `deserialize()` explicitly retries this schema for existing
/// graph fixtures and caches.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithoutPrecision {
    nodes: Vec<SerializableNode>,
    edges: Vec<SerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_lower_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    embeddings: HashMap<String, Vec<f32>>,
}

/// The bincode schema from before embeddings were externalized.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithoutEmbeddings {
    nodes: Vec<SerializableNode>,
    edges: Vec<SerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_lower_index: HashMap<String, Vec<u32>>,
}

/// The pre-name-lower-index form of the post-embedding schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithoutEmbeddingsAndNameLower {
    nodes: Vec<SerializableNode>,
    edges: Vec<SerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
}

/// Edge metadata used by artifacts written before flow-channel fields existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyEdgeMetadata {
    call_count: Option<usize>,
    variable_name: Option<String>,
    confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum LegacyEdgeType {
    Call,
    DataDependency,
    Inheritance,
    Import,
    Containment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyEdge {
    edge_type: LegacyEdgeType,
    metadata: LegacyEdgeMetadata,
}

/// Node schema from the period when embeddings were stored inline on nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyNode {
    id: String,
    node_type: NodeType,
    name: String,
    file_path: String,
    byte_range: (usize, usize),
    complexity: u32,
    language: String,
    embedding: Option<Vec<f32>>,
}

/// Node schema immediately before embeddings were externalized.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacyNodeWithoutEmbedding {
    id: String,
    node_type: NodeType,
    name: String,
    file_path: String,
    byte_range: (usize, usize),
    complexity: u32,
    language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacySerializableNode<N> {
    index: u32,
    node: N,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LegacySerializableEdge {
    source: u32,
    target: u32,
    edge: LegacyEdge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithInlineEmbeddings {
    nodes: Vec<LegacySerializableNode<LegacyNode>>,
    edges: Vec<LegacySerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_lower_index: HashMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithoutInlineEmbeddings {
    nodes: Vec<LegacySerializableNode<LegacyNodeWithoutEmbedding>>,
    edges: Vec<LegacySerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_lower_index: HashMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithInlineEmbeddingsWithoutNameLower {
    nodes: Vec<LegacySerializableNode<LegacyNode>>,
    edges: Vec<LegacySerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SerializablePDGWithoutInlineEmbeddingsAndNameLower {
    nodes: Vec<LegacySerializableNode<LegacyNodeWithoutEmbedding>>,
    edges: Vec<LegacySerializableEdge>,
    symbol_index: HashMap<String, u32>,
    file_index: HashMap<String, Vec<u32>>,
    #[serde(default)]
    name_index: HashMap<String, Vec<u32>>,
}

fn convert_legacy_edge(edge: &LegacyEdge) -> Edge {
    Edge {
        edge_type: match edge.edge_type {
            LegacyEdgeType::Call => EdgeType::Call,
            LegacyEdgeType::DataDependency => EdgeType::DataDependency,
            LegacyEdgeType::Inheritance => EdgeType::Inheritance,
            LegacyEdgeType::Import => EdgeType::Import,
            LegacyEdgeType::Containment => EdgeType::Containment,
        },
        metadata: EdgeMetadata {
            call_count: edge.metadata.call_count,
            variable_name: edge.metadata.variable_name.clone(),
            confidence: edge.metadata.confidence,
            channel: None,
            position: None,
        },
    }
}

fn convert_legacy_node(node: &LegacyNode) -> Node {
    Node {
        id: node.id.clone(),
        node_type: node.node_type.clone(),
        name: node.name.clone(),
        file_path: Arc::from(node.file_path.as_str()),
        byte_range: node.byte_range,
        complexity: node.complexity,
        language: node.language.clone(),
    }
}

fn convert_legacy_node_without_embedding(node: &LegacyNodeWithoutEmbedding) -> Node {
    Node {
        id: node.id.clone(),
        node_type: node.node_type.clone(),
        name: node.name.clone(),
        file_path: Arc::from(node.file_path.as_str()),
        byte_range: node.byte_range,
        complexity: node.complexity,
        language: node.language.clone(),
    }
}

fn convert_legacy_edges(edges: &[LegacySerializableEdge]) -> Vec<SerializableEdge> {
    edges
        .iter()
        .map(|serialized| SerializableEdge {
            source: serialized.source,
            target: serialized.target,
            edge: convert_legacy_edge(&serialized.edge),
        })
        .collect()
}

impl SerializablePDGWithInlineEmbeddings {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let nodes = self
            .nodes
            .iter()
            .map(|serialized| SerializableNode {
                index: serialized.index,
                node: convert_legacy_node(&serialized.node),
            })
            .collect();
        let embeddings = self
            .nodes
            .iter()
            .filter_map(|serialized| {
                serialized
                    .node
                    .embedding
                    .clone()
                    .map(|embedding| (serialized.node.id.clone(), embedding))
            })
            .collect();
        SerializablePDG {
            nodes,
            edges: convert_legacy_edges(&self.edges),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: self.name_lower_index.clone(),
            embeddings,
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDGWithoutInlineEmbeddings {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG {
            nodes: self
                .nodes
                .iter()
                .map(|serialized| SerializableNode {
                    index: serialized.index,
                    node: convert_legacy_node_without_embedding(&serialized.node),
                })
                .collect(),
            edges: convert_legacy_edges(&self.edges),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: self.name_lower_index.clone(),
            embeddings: HashMap::default(),
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDGWithInlineEmbeddingsWithoutNameLower {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let nodes = self
            .nodes
            .iter()
            .map(|serialized| SerializableNode {
                index: serialized.index,
                node: convert_legacy_node(&serialized.node),
            })
            .collect();
        let embeddings = self
            .nodes
            .iter()
            .filter_map(|serialized| {
                serialized
                    .node
                    .embedding
                    .clone()
                    .map(|embedding| (serialized.node.id.clone(), embedding))
            })
            .collect();
        SerializablePDG {
            nodes,
            edges: convert_legacy_edges(&self.edges),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: HashMap::default(),
            embeddings,
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDGWithoutInlineEmbeddingsAndNameLower {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG {
            nodes: self
                .nodes
                .iter()
                .map(|serialized| SerializableNode {
                    index: serialized.index,
                    node: convert_legacy_node_without_embedding(&serialized.node),
                })
                .collect(),
            edges: convert_legacy_edges(&self.edges),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: HashMap::default(),
            embeddings: HashMap::default(),
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}
// ---------------------------------------------------------------------------
// Borrowed serialization shim
// ---------------------------------------------------------------------------
//
// `SerializablePDG::from_pdg` used to deep-clone every node and edge (`Clone`)
// into the serialization buffer, doubling peak memory for large PDGs. The
// borrowed shim below serializes directly from the live graph — node/edge/key
// maps are referenced rather than cloned — while producing byte-for-byte the
// same shape `bincode::deserialize::<SerializablePDG>` expects on the read
// side. Its fields mirror `SerializablePDG` in declaration order so bincode's
// positional encoding lines up exactly.

/// A node reference used only for serialization (no deep clone of the weight).
#[derive(Serialize)]
struct SerializableNodeRef<'a> {
    index: u32,
    node: &'a Node,
}

/// An edge reference used only for serialization (no deep clone of the weight).
#[derive(Serialize)]
struct SerializableEdgeRef<'a> {
    source: u32,
    target: u32,
    edge: &'a Edge,
}

/// Borrowed variant of [`SerializablePDG`] used for clone-free serialization.
#[derive(Serialize)]
struct SerializablePDGRef<'a> {
    nodes: Vec<SerializableNodeRef<'a>>,
    edges: Vec<SerializableEdgeRef<'a>>,
    symbol_index: HashMap<&'a str, u32>,
    file_index: HashMap<&'a str, Vec<u32>>,
    name_index: HashMap<&'a str, Vec<u32>>,
    name_lower_index: HashMap<&'a str, Vec<u32>>,
    embeddings: HashMap<&'a str, &'a Vec<f32>>,
    precision_symbols: &'a HashSet<String>,
}

impl SerializablePDGRef<'_> {
    /// Build a borrowed serialization shim directly from graph references,
    /// avoiding the per-node/per-edge `clone()` that `SerializablePDG::from_pdg`
    /// performed.
    fn from_pdg(pdg: &ProgramDependenceGraph) -> SerializablePDGRef<'_> {
        let nodes = pdg
            .graph
            .node_indices()
            .map(|idx| SerializableNodeRef {
                index: idx.index() as u32,
                node: &pdg.graph[idx],
            })
            .collect();

        let edges = pdg
            .graph
            .edge_indices()
            .map(|eidx| {
                let (source, target) = pdg
                    .graph
                    .edge_endpoints(eidx)
                    .expect("Edge endpoints must exist");
                SerializableEdgeRef {
                    source: source.index() as u32,
                    target: target.index() as u32,
                    edge: &pdg.graph[eidx],
                }
            })
            .collect();

        let symbol_index = pdg
            .symbol_index
            .iter()
            .map(|(k, v)| (k.as_str(), v.index() as u32))
            .collect();
        let file_index = pdg
            .file_index
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_index = pdg
            .name_index
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_lower_index = pdg
            .name_lower_index
            .iter()
            .map(|(k, v)| (k.as_str(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();

        let embeddings = pdg
            .embedding_store
            .embeddings
            .iter()
            .map(|(k, v)| (k.as_str(), v))
            .collect();

        SerializablePDGRef {
            nodes,
            edges,
            symbol_index,
            file_index,
            name_index,
            name_lower_index,
            embeddings,
            precision_symbols: &pdg.precision_symbols,
        }
    }
}

impl SerializablePDGWithoutPrecision {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: self.name_lower_index.clone(),
            embeddings: self.embeddings.clone(),
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDGWithoutEmbeddings {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: self.name_lower_index.clone(),
            embeddings: HashMap::default(),
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDGWithoutEmbeddingsAndNameLower {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG {
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: HashMap::default(),
            embeddings: HashMap::default(),
            precision_symbols: HashSet::default(),
        }
        .to_pdg()
    }
}

impl SerializablePDG {
    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let mut pdg = ProgramDependenceGraph::new();
        let index_map = self.restore_nodes(&mut pdg);
        self.restore_indexes(&mut pdg, &index_map);
        self.restore_edges(&mut pdg, &index_map)?;
        self.restore_embeddings(&mut pdg);
        self.restore_precision_symbols(&mut pdg);
        Self::rebuild_name_file_index(&mut pdg);

        if pdg.trigram_index.is_empty() {
            pdg.rebuild_trigram_index();
        }

        Ok(pdg)
    }

    fn restore_nodes(&self, pdg: &mut ProgramDependenceGraph) -> HashMap<u32, NodeId> {
        self.nodes
            .iter()
            .map(|serialized| {
                let node_id = pdg.graph.add_node(serialized.node.clone());
                (serialized.index, node_id)
            })
            .collect()
    }

    fn restore_indexes(&self, pdg: &mut ProgramDependenceGraph, index_map: &HashMap<u32, NodeId>) {
        Self::restore_symbol_index(&mut pdg.symbol_index, &self.symbol_index, index_map);
        Self::restore_node_index(&mut pdg.file_index, &self.file_index, index_map);
        Self::restore_node_index(&mut pdg.name_index, &self.name_index, index_map);
        Self::restore_node_index(&mut pdg.name_lower_index, &self.name_lower_index, index_map);

        if pdg.name_index.is_empty() {
            Self::rebuild_name_indexes(pdg);
        }
    }

    fn restore_symbol_index(
        destination: &mut HashMap<String, NodeId>,
        source: &HashMap<String, u32>,
        index_map: &HashMap<u32, NodeId>,
    ) {
        destination.reserve(source.len());
        for (symbol, old_index) in source {
            if let Some(&node_id) = index_map.get(old_index) {
                destination.insert(symbol.clone(), node_id);
            }
        }
    }

    fn restore_node_index(
        destination: &mut HashMap<String, Vec<NodeId>>,
        source: &HashMap<String, Vec<u32>>,
        index_map: &HashMap<u32, NodeId>,
    ) {
        destination.reserve(source.len());
        for (name, old_indices) in source {
            let node_ids: Vec<NodeId> = old_indices
                .iter()
                .filter_map(|index| index_map.get(index).copied())
                .collect();
            if !node_ids.is_empty() {
                destination.insert(name.clone(), node_ids);
            }
        }
    }

    fn rebuild_name_indexes(pdg: &mut ProgramDependenceGraph) {
        let node_count = pdg.graph.node_count();
        pdg.name_index.reserve(node_count);
        pdg.name_lower_index.reserve(node_count);
        for node_id in pdg.graph.node_indices() {
            if let Some(node) = pdg.graph.node_weight(node_id) {
                pdg.name_index
                    .entry(node.name.clone())
                    .or_default()
                    .push(node_id);
                pdg.name_lower_index
                    .entry(node.name.to_lowercase())
                    .or_default()
                    .push(node_id);
            }
        }
    }

    fn restore_edges(
        &self,
        pdg: &mut ProgramDependenceGraph,
        index_map: &HashMap<u32, NodeId>,
    ) -> Result<(), String> {
        for serialized in &self.edges {
            let source = index_map
                .get(&serialized.source)
                .ok_or_else(|| format!("Missing source {}", serialized.source))?;
            let target = index_map
                .get(&serialized.target)
                .ok_or_else(|| format!("Missing target {}", serialized.target))?;
            pdg.graph
                .add_edge(*source, *target, serialized.edge.clone());
        }
        Ok(())
    }

    fn restore_embeddings(&self, pdg: &mut ProgramDependenceGraph) {
        for (node_id, embedding) in &self.embeddings {
            pdg.embedding_store.insert(node_id, embedding.clone());
        }
    }

    fn restore_precision_symbols(&self, pdg: &mut ProgramDependenceGraph) {
        let restored = self
            .precision_symbols
            .iter()
            .filter(|symbol| pdg.find_by_symbol(symbol).is_some())
            .cloned()
            .collect::<Vec<_>>();
        pdg.precision_symbols.extend(restored);
    }

    fn rebuild_name_file_index(pdg: &mut ProgramDependenceGraph) {
        for node_id in pdg.graph.node_indices() {
            if let Some(node) = pdg.graph.node_weight(node_id) {
                pdg.name_file_index
                    .insert((node.name.clone(), node.file_path.to_string()), node_id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ProgramDependenceGraph
// ---------------------------------------------------------------------------

/// The Program Dependence Graph (PDG) representing code structure and relationships.
///
/// The PDG is the core data structure of LeIndex. It maintains:
///
/// - **Nodes**: Code entities (functions, classes, methods, variables, modules)
/// - **Edges**: Relationships between entities (calls, data flow, inheritance, imports, containment)
/// - **Indexes**: Multiple indexes for efficient lookups by symbol, file, or name
///
/// The graph uses `petgraph::StableGraph` internally, which provides:
/// - Stable node/edge indices across modifications
/// - Efficient traversal and querying
/// - Support for parallel edge handling
///
/// # Indexes
///
/// The PDG maintains several indexes for O(1) lookups:
/// - `symbol_index`: Maps fully qualified IDs to node IDs
/// - `file_index`: Maps file paths to all nodes in that file
/// - `name_index`: Maps symbol names to nodes (exact match)
/// - `name_lower_index`: Maps lowercase names for case-insensitive search
pub struct ProgramDependenceGraph {
    /// The underlying stable graph storing nodes and edges.
    pub(crate) graph: StableGraph<Node, Edge>,

    /// Maps node.id (format: "file_path:qualified_name") → NodeId
    ///
    /// Used for O(1) lookup of nodes by their fully qualified identifier.
    pub(crate) symbol_index: HashMap<String, NodeId>,

    /// Maps file_path → `Vec<NodeId>`
    ///
    /// Used to quickly find all nodes defined in a specific file.
    pub(crate) file_index: HashMap<String, Vec<NodeId>>,

    /// Maps node.name (exact) → `Vec<NodeId>`
    ///
    /// Used for finding nodes by their human-readable name.
    pub(crate) name_index: HashMap<String, Vec<NodeId>>,

    /// Maps lowercase node.name → `Vec<NodeId>`
    ///
    /// Enables O(1) case-insensitive lookups without scanning the entire graph.
    /// This eliminates the O(n) scan that would otherwise be needed for
    /// case-insensitive searches like `find_by_name_in_file`.
    pub(crate) name_lower_index: HashMap<String, Vec<NodeId>>,

    /// Externalized embedding storage.
    ///
    /// Embeddings are stored here rather than inline in `Node` to reduce memory
    /// usage. At 50k nodes with 1536-dim embeddings, inline storage would add
    /// ~300MB; this optional store is populated on demand.
    pub embedding_store: EmbeddingStore,

    /// Leiden community per node (roadmap Part IV), populated post-
    /// construction by `graph::community::detect_communities`.
    #[cfg(feature = "community")]
    pub communities: HashMap<NodeId, u32>,

    /// Stable node IDs confirmed by SCIP precision ingest.
    ///
    /// Strings are used instead of transient `NodeId` values so the marker
    /// remains meaningful across graph reloads and generation rebuilds.
    precision_symbols: HashSet<String>,

    /// O(1) lookup by (name, file_path) pair.
    ///
    /// Used by `find_by_name_in_file()` when a file hint is provided,
    /// replacing the linear scan through `name_index` candidates.
    /// Populated during `add_node()`, cleaned up in `remove_node()`.
    name_file_index: HashMap<(String, String), NodeId>,

    /// Trigram index for accelerating fuzzy node lookups.
    ///
    /// Maps 3-character substrings to sets of node indices, enabling
    /// `fuzzy_find_node` to skip nodes that share no trigrams with the query.
    /// Built lazily on first fuzzy search, or eagerly during indexing.
    /// Persisted alongside the PDG in SQLite.
    trigram_index: TrigramIndex,

    /// Process-unique revision of the node set. Assigned when the graph is
    /// created and replaced with a fresh value on every node mutation, so two
    /// graphs (or two states of one graph) never share a revision unless one is
    /// an unmodified clone of the other.
    revision: u64,

    /// Lower-cased distinct names/paths, memoised against `revision`.
    name_corpus: Mutex<Option<(u64, Arc<NameCorpus>)>>,
}

static NEXT_PDG_REVISION: AtomicU64 = AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_PDG_REVISION.fetch_add(1, AtomicOrdering::Relaxed)
}

/// Distinct lower-cased node names and file paths of a PDG.
///
/// Import validation probes these with `contains` instead of lower-casing every
/// node per import. Built once per PDG revision and shared via `Arc`.
#[derive(Debug, Default)]
pub struct NameCorpus {
    /// Distinct lower-cased node names.
    pub names: Vec<String>,
    /// Distinct lower-cased node file paths.
    pub files: Vec<String>,
}

impl Clone for ProgramDependenceGraph {
    fn clone(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            symbol_index: self.symbol_index.clone(),
            file_index: self.file_index.clone(),
            name_index: self.name_index.clone(),
            name_lower_index: self.name_lower_index.clone(),
            embedding_store: self.embedding_store.clone(),
            #[cfg(feature = "community")]
            communities: self.communities.clone(),
            precision_symbols: self.precision_symbols.clone(),
            name_file_index: self.name_file_index.clone(),
            trigram_index: self.trigram_index.clone(),
            revision: self.revision,
            name_corpus: Mutex::new(
                self.name_corpus
                    .lock()
                    .ok()
                    .and_then(|cached| cached.clone()),
            ),
        }
    }
}

impl ProgramDependenceGraph {
    /// Creates a new, empty ProgramDependenceGraph.
    pub fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            symbol_index: HashMap::default(),
            file_index: HashMap::default(),
            name_index: HashMap::default(),
            name_lower_index: HashMap::default(),
            embedding_store: EmbeddingStore::new(),
            #[cfg(feature = "community")]
            communities: HashMap::default(),
            precision_symbols: HashSet::default(),
            name_file_index: HashMap::default(),
            trigram_index: TrigramIndex::new(),
            revision: next_revision(),
            name_corpus: Mutex::new(None),
        }
    }

    /// Revision of the node set; changes whenever nodes are added, removed or
    /// mutated. Suitable as a cache key for data derived from node contents.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn touch(&mut self) {
        self.revision = next_revision();
    }

    /// Distinct lower-cased node names and file paths, built once per
    /// [`revision`](Self::revision) and shared by every caller.
    pub fn name_corpus(&self) -> Arc<NameCorpus> {
        let mut cached = match self.name_corpus.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((revision, corpus)) = cached.as_ref() {
            if *revision == self.revision {
                return Arc::clone(corpus);
            }
        }
        let mut names: HashSet<String> = HashSet::default();
        let mut files: HashSet<String> = HashSet::default();
        for node in self.graph.node_weights() {
            names.insert(node.name.to_lowercase());
            files.insert(node.file_path.to_lowercase());
        }
        let corpus = Arc::new(NameCorpus {
            names: names.into_iter().collect(),
            files: files.into_iter().collect(),
        });
        *cached = Some((self.revision, Arc::clone(&corpus)));
        corpus
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Adds a node to the graph and updates all indexes.
    ///
    /// This method inserts the node into the underlying graph and updates
    /// all internal indexes (symbol_index, file_index, name_index, name_lower_index)
    /// to ensure O(1) lookups remain available.
    ///
    /// # Arguments
    ///
    /// * `node` - The node to add to the graph
    ///
    /// # Returns
    ///
    /// The NodeId assigned to the newly added node.
    /// Ensure every file represented in the PDG has a `FileSummary` node.
    /// Idempotent + resume-proof: call once after the PDG is finalized (fresh
    /// build OR resumed from storage), before embedding. The per-file
    /// `merge_pdgs` loop only fires for freshly-parsed files; on a resume most
    /// files are loaded from storage and would otherwise miss their summary.
    /// (conceptual-recall fix.)
    pub fn ensure_file_summary_nodes(&mut self) {
        use std::collections::{HashMap, HashSet};
        let mut file_lang: HashMap<String, String> = HashMap::default();
        let mut have_summary: HashSet<String> = HashSet::default();
        for ni in self.node_indices() {
            if let Some(n) = self.get_node(ni) {
                let fp = n.file_path.to_string();
                if matches!(n.node_type, NodeType::FileSummary) {
                    have_summary.insert(fp);
                } else {
                    file_lang.entry(fp).or_insert_with(|| n.language.clone());
                }
            }
        }
        for (fp, lang) in file_lang {
            if !have_summary.contains(&fp) {
                self.add_node(Node::new_file_summary(&fp, &lang));
            }
        }
    }

    /// Add a node to the graph, returning its stable `NodeId`.
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let name = node.name.clone();
        let node_id_str = node.id.clone();
        let file_path = Arc::clone(&node.file_path);
        let id = self.add_node_without_trigrams(node);
        // Update trigram index incrementally
        self.trigram_index
            .add_node(id, &name, &node_id_str, &file_path);
        id
    }

    /// Pre-size the node graph and per-node lookup indexes for `additional`
    /// upcoming insertions (bulk loads know the count up front).
    pub fn reserve_nodes(&mut self, additional: usize) {
        self.graph.reserve_nodes(additional);
        self.symbol_index.reserve(additional);
        self.name_index.reserve(additional);
        self.name_lower_index.reserve(additional);
        self.name_file_index.reserve(additional);
    }

    /// Add a node to the graph and every lookup index *except* the trigram
    /// index.
    ///
    /// Bulk loading uses this: the trigram index costs ~300 posting inserts
    /// per node, and a persisted copy replaces it wholesale afterwards
    /// ([`set_trigram_index`](Self::set_trigram_index)), so building it
    /// incrementally was pure waste (~250 ms per cold start on a 28k-node
    /// graph). Callers must install or [rebuild](Self::rebuild_trigram_index)
    /// the trigram index when done.
    pub fn add_node_without_trigrams(&mut self, node: Node) -> NodeId {
        let symbol = node.id.clone();
        let name = node.name.clone();
        let lower = name.to_lowercase();
        let file_path = Arc::clone(&node.file_path);
        let id = self.graph.add_node(node);
        self.touch();

        self.symbol_index.insert(symbol, id);
        // Look up by &str first: most nodes share a file / name with an earlier
        // one, and `entry(key.to_string())` would allocate on every call.
        match self.file_index.get_mut(&*file_path) {
            Some(nodes) => nodes.push(id),
            None => {
                self.file_index.insert(file_path.to_string(), vec![id]);
            }
        }
        match self.name_index.get_mut(name.as_str()) {
            Some(nodes) => nodes.push(id),
            None => {
                self.name_index.insert(name.clone(), vec![id]);
            }
        }
        match self.name_lower_index.get_mut(lower.as_str()) {
            Some(nodes) => nodes.push(id),
            None => {
                self.name_lower_index.insert(lower, vec![id]);
            }
        }
        self.name_file_index
            .insert((name, file_path.to_string()), id);
        id
    }

    /// Add an edge. Returns the EdgeId directly (never fails silently).
    /// Callers should validate that `from` and `to` exist before calling.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, edge: Edge) -> EdgeId {
        debug_assert!(
            self.graph.contains_node(from) && self.graph.contains_node(to),
            "add_edge called with invalid NodeId(s): from={:?} to={:?}",
            from,
            to
        );
        self.graph.add_edge(from, to, edge)
    }

    /// Removes a node from the graph and updates all indexes.
    ///
    /// This method removes the node from the underlying graph and cleans up
    /// all references in the internal indexes (symbol_index, file_index,
    /// name_index, name_lower_index).
    ///
    /// # Arguments
    ///
    /// * `node_id` - The ID of the node to remove
    ///
    /// # Returns
    ///
    /// The removed node if it existed, or None if not found.
    pub fn remove_node(&mut self, node_id: NodeId) -> Option<Node> {
        if let Some(node) = self.graph.remove_node(node_id) {
            self.touch();
            self.symbol_index.remove(&node.id);
            self.precision_symbols.remove(&node.id);
            self.embedding_store.remove(&node.id);
            let remove_file_entry = if let Some(v) = self.file_index.get_mut(&*node.file_path) {
                v.retain(|&id| id != node_id);
                v.is_empty()
            } else {
                false
            };
            if remove_file_entry {
                self.file_index.remove(&*node.file_path);
            }

            let remove_name_entry = if let Some(v) = self.name_index.get_mut(&node.name) {
                v.retain(|&id| id != node_id);
                v.is_empty()
            } else {
                false
            };
            if remove_name_entry {
                self.name_index.remove(&node.name);
            }

            let lower_name = node.name.to_lowercase();
            let remove_lower_name_entry =
                if let Some(v) = self.name_lower_index.get_mut(&lower_name) {
                    v.retain(|&id| id != node_id);
                    v.is_empty()
                } else {
                    false
                };
            if remove_lower_name_entry {
                self.name_lower_index.remove(&lower_name);
            }
            self.name_file_index
                .remove(&(node.name.clone(), node.file_path.to_string()));

            // Update trigram index
            self.trigram_index
                .remove_node(node_id, &node.name, &node.id, &node.file_path);

            Some(node)
        } else {
            None
        }
    }

    /// Removes an edge from the graph.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID of the edge to remove
    ///
    /// # Returns
    ///
    /// The removed edge if it existed, or None if not found.
    pub fn remove_edge(&mut self, id: EdgeId) -> Option<Edge> {
        self.graph.remove_edge(id)
    }

    /// Removes all nodes belonging to a specific file.
    ///
    /// This is useful when re-indexing a file - first remove all existing
    /// nodes for that file, then add the newly parsed nodes.
    ///
    /// # Arguments
    ///
    /// * `file_path` - The path of the file whose nodes should be removed
    pub fn remove_file(&mut self, file_path: &str) {
        let ids = self.nodes_in_file(file_path);
        for id in ids {
            self.remove_node(id);
        }
        self.file_index.remove(file_path);
    }

    // -----------------------------------------------------------------------
    // Read access
    // -----------------------------------------------------------------------

    /// Retrieves an immutable reference to a node by its ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID of the node to retrieve
    ///
    /// # Returns
    ///
    /// An optional reference to the node if it exists.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.graph.node_weight(id)
    }

    /// Retrieves a mutable reference to a node by its ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID of the node to retrieve
    ///
    /// # Returns
    ///
    /// An optional mutable reference to the node if it exists.
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.touch();
        self.graph.node_weight_mut(id)
    }

    /// Move a node to a different `file_path`, maintaining every index the
    /// mutation affects (`file_index`, `name_file_index`, the trigram
    /// index).
    ///
    /// A bare weight write (`get_node_mut(...).file_path = ...`, or through
    /// `node_weights_mut`) leaves `file_index["<old>"]` holding this NodeId:
    /// `remove_file` for the old file would then reap the node under its old
    /// identity, and a later `remove_node` — looking it up under the NEW
    /// path, which it was never inserted under — would leave a dangling
    /// entry that petgraph slot recycling can alias onto an unrelated node.
    /// Returns `false` when the node does not exist or already carries the
    /// path.
    pub fn repath_node(&mut self, node_id: NodeId, new_path: &str) -> bool {
        let (name, node_id_str, old_path) = {
            let Some(node) = self.graph.node_weight_mut(node_id) else {
                return false;
            };
            if node.file_path.as_ref() == new_path {
                return false;
            }
            let name = node.name.clone();
            let node_id_str = node.id.clone();
            let old_path = std::sync::Arc::clone(&node.file_path);
            node.file_path = std::sync::Arc::from(new_path);
            (name, node_id_str, old_path)
        };
        self.touch();
        // file_index: leave the old entry, join the new one.
        if let Some(ids) = self.file_index.get_mut(&*old_path) {
            ids.retain(|&id| id != node_id);
            if ids.is_empty() {
                self.file_index.remove(&*old_path);
            }
        }
        match self.file_index.get_mut(new_path) {
            Some(ids) => ids.push(node_id),
            None => {
                self.file_index.insert(new_path.to_string(), vec![node_id]);
            }
        }
        // (name, file_path) lookup key moves with the path.
        self.name_file_index
            .remove(&(name.clone(), old_path.to_string()));
        self.name_file_index
            .insert((name.clone(), new_path.to_string()), node_id);
        // The trigram index indexes the path's trigrams for fuzzy lookup.
        self.trigram_index
            .remove_node(node_id, &name, &node_id_str, &old_path);
        self.trigram_index
            .add_node(node_id, &name, &node_id_str, new_path);
        true
    }

    /// Returns a mutable slice of all node weights.
    /// Used for bulk node mutations (e.g., external node normalization).
    pub fn node_weights_mut(&mut self) -> impl Iterator<Item = &mut Node> {
        self.touch();
        self.graph.node_weights_mut()
    }

    /// Retrieves a reference to an edge by its ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID of the edge
    ///
    /// # Returns
    ///
    /// An optional reference to the edge if it exists.
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> {
        self.graph.edge_weight(id)
    }

    /// Retrieves a mutable edge reference for metadata upgrades.
    pub fn get_edge_mut(&mut self, id: EdgeId) -> Option<&mut Edge> {
        self.graph.edge_weight_mut(id)
    }

    /// Mark a stable node identifier as confirmed by SCIP.
    pub fn mark_precision_symbol(&mut self, node_id: impl Into<String>) {
        self.precision_symbols.insert(node_id.into());
    }

    /// Return whether a stable node identifier has SCIP confirmation.
    pub fn is_precision_symbol(&self, node_id: &str) -> bool {
        self.precision_symbols.contains(node_id)
    }

    /// Read access to the full precision-marker set (shared, not cloned).
    pub fn precision_symbols(&self) -> &HashSet<String> {
        &self.precision_symbols
    }

    /// Remove every precision marker whose node id matches `keep` returning
    /// false. Callers that bulk-revoke markers after file invalidations use
    /// this instead of cloning the set.
    pub fn retain_precision_symbols<F: FnMut(&String) -> bool>(&mut self, keep: F) {
        self.precision_symbols.retain(keep);
    }

    /// Returns the total number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Returns the total number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Returns the total number of files indexed in the graph.
    pub fn file_count(&self) -> usize {
        self.file_index
            .values()
            .filter(|node_ids| !node_ids.is_empty())
            .count()
    }

    /// Returns an iterator over all node IDs in the graph.
    pub fn node_indices(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.graph.node_indices()
    }

    /// Returns an iterator over all edge IDs in the graph.
    pub fn edge_indices(&self) -> impl Iterator<Item = EdgeId> + '_ {
        self.graph.edge_indices()
    }

    /// Returns the source and target nodes for a given edge.
    ///
    /// # Arguments
    ///
    /// * `edge_id` - The ID of the edge
    ///
    /// # Returns
    ///
    /// An optional tuple of (source_node, target_node) if the edge exists.
    pub fn edge_endpoints(&self, edge_id: EdgeId) -> Option<(NodeId, NodeId)> {
        self.graph.edge_endpoints(edge_id)
    }

    /// Returns all outgoing neighbor nodes from the given node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The ID of the node to get neighbors for
    ///
    /// # Returns
    ///
    /// A vector of node IDs representing all outgoing neighbors.
    pub fn neighbors(&self, node_id: NodeId) -> Vec<NodeId> {
        self.graph.neighbors(node_id).collect()
    }

    /// Returns all incoming predecessor nodes to the given node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The ID of the node to get predecessors for
    ///
    /// # Returns
    ///
    /// A vector of node IDs representing all incoming predecessors.
    pub fn predecessors(&self, node_id: NodeId) -> Vec<NodeId> {
        use petgraph::Direction;
        self.graph
            .neighbors_directed(node_id, Direction::Incoming)
            .collect()
    }

    /// Nodes reached from `node_id` through edges of a specific type in the
    /// given direction, deduplicated.
    ///
    /// "Callers"/"callees" semantics: relationship renders must report CALL
    /// edges, not every edge type — data-flow heuristics (every function
    /// taking/returning `String` connected to every other) and containment
    /// (an impl block "calling" its own methods) previously leaked into
    /// caller lists, conflating unrelated same-typed symbols (N-04).
    pub fn neighbors_by_edge_type(
        &self,
        node_id: NodeId,
        edge_type: EdgeType,
        direction: petgraph::Direction,
    ) -> Vec<NodeId> {
        use petgraph::visit::EdgeRef;
        let mut seen = HashSet::default();
        self.graph
            .edges_directed(node_id, direction)
            .filter(|edge| edge.weight().edge_type == edge_type)
            .map(|edge| match direction {
                petgraph::Direction::Outgoing => edge.target(),
                petgraph::Direction::Incoming => edge.source(),
            })
            .filter(|id| seen.insert(*id))
            .collect()
    }

    /// Returns the count of incoming predecessor nodes.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The ID of the node to count predecessors for
    ///
    /// # Returns
    ///
    /// The number of incoming edges to this node.
    pub fn predecessor_count(&self, node_id: NodeId) -> usize {
        use petgraph::Direction;
        self.graph
            .neighbors_directed(node_id, Direction::Incoming)
            .count()
    }

    // -----------------------------------------------------------------------
    // Trigram index access
    // -----------------------------------------------------------------------

    /// Get a reference to the trigram index.
    ///
    /// The trigram index is maintained incrementally as nodes are added/removed.
    /// It can also be rebuilt from scratch with `rebuild_trigram_index()`.
    pub fn trigram_index(&self) -> &TrigramIndex {
        &self.trigram_index
    }

    /// Rebuild the trigram index from scratch from all current nodes.
    ///
    /// This is useful after bulk operations that bypass `add_node`/`remove_node`,
    /// such as deserialization or loading from storage.
    pub fn rebuild_trigram_index(&mut self) {
        self.trigram_index = TrigramIndex::build_from_pdg(self);
    }

    /// Set the trigram index (used when loading from storage).
    pub fn set_trigram_index(&mut self, index: TrigramIndex) {
        self.trigram_index = index;
    }
}

fn deserialize_schema<T>(data: &[u8], errors: &mut Vec<String>) -> Option<ProgramDependenceGraph>
where
    T: DeserializeOwned,
    T: IntoPdg,
{
    match bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize::<T>(data)
    {
        Ok(serialized) => match serialized.to_pdg() {
            Ok(pdg) => Some(pdg),
            Err(error) => {
                errors.push(error);
                None
            }
        },
        Err(error) => {
            errors.push(error.to_string());
            None
        }
    }
}

trait IntoPdg {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String>;
}

impl IntoPdg for SerializablePDG {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDG::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithoutPrecision {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithoutPrecision::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithoutEmbeddings {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithoutEmbeddings::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithoutEmbeddingsAndNameLower {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithoutEmbeddingsAndNameLower::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithInlineEmbeddings {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithInlineEmbeddings::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithoutInlineEmbeddings {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithoutInlineEmbeddings::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithInlineEmbeddingsWithoutNameLower {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithInlineEmbeddingsWithoutNameLower::to_pdg(&self)
    }
}

impl IntoPdg for SerializablePDGWithoutInlineEmbeddingsAndNameLower {
    fn to_pdg(self) -> Result<ProgramDependenceGraph, String> {
        SerializablePDGWithoutInlineEmbeddingsAndNameLower::to_pdg(&self)
    }
}

impl Default for ProgramDependenceGraph {
    fn default() -> Self {
        Self::new()
    }
}

// Internal direction enum (not re-exporting petgraph's Direction to keep API clean)
enum Direction {
    Forward,
    Backward,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "pdg_test.rs"]
mod tests;
