// Program Dependence Graph — Rewrite
//
// Key changes from original:
//   - `EdgeType::Containment` added (Class→Method, Module→Function structural edges)
//   - `TraversalConfig` drives all impact/traversal methods — no more unbounded variants
//   - Embeddings externalized to `EmbeddingStore` (separate HashMap<NodeId, Vec<f32>>)
//   - `find_by_name_in_file` O(n) fallbacks eliminated via normalized secondary index
//   - `add_edge` returns `EdgeId` directly (was misleadingly Option<EdgeId>)
//   - All public traversal methods take `TraversalConfig` — callers must be explicit

use petgraph::stable_graph::StableGraph;
use petgraph::visit::EdgeRef;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::graph::trigram::TrigramIndex;

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

    /// Synthetic per-file summary node (conceptual-recall fix). Embeds the
    /// file's leading doc comment + the names of its top-level items as a
    /// single retrievable unit, so a conceptual NL query can match a file by
    /// its *purpose* even when no individual function does. `byte_range=(0,0)`.
    FileSummary,
}

/// Edge types — now includes Containment for structural (non-semantic) relationships.
///
/// Filtering guidance for callers:
///   - Call + DataDependency + Inheritance = semantic graph (use for impact analysis)
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
/// | LLM context window (broad)  | 5         | 150       | Call, DataDependency, Inheritance|
/// | Impact analysis (full)      | None      | 500       | Call, DataDependency, Inheritance|
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

    /// Broad semantic analysis — includes inheritance, moderate limits.
    pub fn for_semantic_analysis() -> Self {
        Self {
            max_depth: Some(5),
            max_nodes: Some(150),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
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

    /// Full impact analysis — all semantic edges, hard node cap.
    pub fn for_impact_analysis() -> Self {
        Self {
            max_depth: None,
            max_nodes: Some(500),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
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
}

impl SerializablePDG {
    fn from_pdg(pdg: &ProgramDependenceGraph) -> Self {
        let nodes = pdg
            .graph
            .node_indices()
            .map(|idx| SerializableNode {
                index: idx.index() as u32,
                node: pdg.graph[idx].clone(),
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
                SerializableEdge {
                    source: source.index() as u32,
                    target: target.index() as u32,
                    edge: pdg.graph[eidx].clone(),
                }
            })
            .collect();

        let symbol_index = pdg
            .symbol_index
            .iter()
            .map(|(k, v)| (k.clone(), v.index() as u32))
            .collect();
        let file_index = pdg
            .file_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_index = pdg
            .name_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_lower_index = pdg
            .name_lower_index
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();

        Self {
            nodes,
            edges,
            symbol_index,
            file_index,
            name_index,
            name_lower_index,
            embeddings: pdg.embedding_store.embeddings.clone(),
        }
    }

    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let mut pdg = ProgramDependenceGraph::new();
        let index_map = self.restore_nodes(&mut pdg);
        self.restore_indexes(&mut pdg, &index_map);
        self.restore_edges(&mut pdg, &index_map)?;
        self.restore_embeddings(&mut pdg);
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

    /// Maps file_path → Vec<NodeId>
    ///
    /// Used to quickly find all nodes defined in a specific file.
    pub(crate) file_index: HashMap<String, Vec<NodeId>>,

    /// Maps node.name (exact) → Vec<NodeId>
    ///
    /// Used for finding nodes by their human-readable name.
    pub(crate) name_index: HashMap<String, Vec<NodeId>>,

    /// Maps lowercase node.name → Vec<NodeId>
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

    /// O(1) lookup by (name, file_path) pair.
    ///
    /// Used by `find_by_name_in_file()` when a file hint is provided,
    /// replacing the linear scan through `name_index` candidates.
    /// Populated during `add_node()`, cleaned up in `remove_node()`.
    name_file_index: HashMap<(String, String), NodeId>,

    /// Reusable scratch buffer for BFS neighbor collection.
    ///
    /// Avoids allocating a new `Vec<NodeId>` on every BFS level iteration.
    /// Cleared at the start of each level, reused across traversals.
    /// Wrapped in `Mutex` because the PDG is shared across threads
    /// (e.g., via `Arc<ProgramDependenceGraph>` in validation handlers).
    bfs_scratch: Mutex<Vec<NodeId>>,

    /// Trigram index for accelerating fuzzy node lookups.
    ///
    /// Maps 3-character substrings to sets of node indices, enabling
    /// `fuzzy_find_node` to skip nodes that share no trigrams with the query.
    /// Built lazily on first fuzzy search, or eagerly during indexing.
    /// Persisted alongside the PDG in SQLite.
    trigram_index: TrigramIndex,
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
            name_file_index: self.name_file_index.clone(),
            bfs_scratch: Mutex::new(Vec::new()),
            trigram_index: self.trigram_index.clone(),
        }
    }
}

impl ProgramDependenceGraph {
    /// Creates a new, empty ProgramDependenceGraph.
    pub fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            symbol_index: HashMap::new(),
            file_index: HashMap::new(),
            name_index: HashMap::new(),
            name_lower_index: HashMap::new(),
            embedding_store: EmbeddingStore::new(),
            name_file_index: HashMap::new(),
            bfs_scratch: Mutex::new(Vec::new()),
            trigram_index: TrigramIndex::new(),
        }
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
        let mut file_lang: HashMap<String, String> = HashMap::new();
        let mut have_summary: HashSet<String> = HashSet::new();
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
        let id = self.graph.add_node(node.clone());
        self.symbol_index.insert(node.id.clone(), id);
        self.file_index
            .entry(node.file_path.to_string())
            .or_default()
            .push(id);
        self.name_index
            .entry(node.name.clone())
            .or_default()
            .push(id);
        self.name_lower_index
            .entry(node.name.to_lowercase())
            .or_default()
            .push(id);
        self.name_file_index
            .insert((node.name.clone(), node.file_path.to_string()), id);

        // Update trigram index incrementally
        self.trigram_index
            .add_node(id, &node.name, &node.id, &node.file_path);

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
            self.symbol_index.remove(&node.id);
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
        self.graph.node_weight_mut(id)
    }

    /// Returns a mutable slice of all node weights.
    /// Used for bulk node mutations (e.g., external node normalization).
    pub fn node_weights_mut(&mut self) -> impl Iterator<Item = &mut Node> {
        self.graph.node_weights_mut()
    }

    /// Retrieves a reference to an edge by its ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The ID of the edge to retrieve
    ///
    /// # Returns
    ///
    /// An optional reference to the edge if it exists.
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> {
        self.graph.edge_weight(id)
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
    // Lookup (all O(1) or O(k) where k = results count)
    // -----------------------------------------------------------------------

    /// Finds a node by its fully qualified symbol ID.
    ///
    /// This performs an O(1) lookup using the symbol_index.
    ///
    /// # Arguments
    ///
    /// * `symbol` - The fully qualified symbol identifier
    ///
    /// # Returns
    ///
    /// An optional NodeId if the symbol exists in the graph.
    pub fn find_by_symbol(&self, symbol: &str) -> Option<NodeId> {
        self.symbol_index.get(symbol).copied()
    }

    /// Finds a node by its ID string (alias for find_by_symbol).
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node identifier string
    ///
    /// # Returns
    ///
    /// An optional NodeId if the node exists in the graph.
    pub fn find_by_id(&self, node_id: &str) -> Option<NodeId> {
        self.symbol_index.get(node_id).copied()
    }

    /// Returns all nodes defined in a specific file.
    ///
    /// This performs an O(1) lookup using the file_index.
    ///
    /// # Arguments
    ///
    /// * `file_path` - The path of the file to query
    ///
    /// # Returns
    ///
    /// A vector of NodeIds for all nodes in the file (empty if file not found).
    pub fn nodes_in_file(&self, file_path: &str) -> Vec<NodeId> {
        self.file_index.get(file_path).cloned().unwrap_or_default()
    }

    /// Finds the first node with the given name (exact match).
    ///
    /// This performs an O(1) lookup using the name_index.
    ///
    /// # Arguments
    ///
    /// * `name` - The symbol name to search for
    ///
    /// # Returns
    ///
    /// An optional NodeId if at least one node with this name exists.
    pub fn find_by_name(&self, name: &str) -> Option<NodeId> {
        self.name_index
            .get(name)
            .and_then(|ids| ids.first().copied())
    }

    /// Finds all nodes with the given name (exact match).
    ///
    /// This performs an O(1) lookup using the name_index.
    ///
    /// # Arguments
    ///
    /// * `name` - The symbol name to search for
    ///
    /// # Returns
    ///
    /// A vector of all NodeIds with this name (empty if none found).
    pub fn find_all_by_name(&self, name: &str) -> Vec<NodeId> {
        self.name_index.get(name).cloned().unwrap_or_default()
    }

    /// Find by name with optional file hint.
    /// All lookups are index-backed — no O(n) scans.
    pub fn find_by_name_in_file(&self, name: &str, file_hint: Option<&str>) -> Option<NodeId> {
        if let Some(file_path) = file_hint {
            if let Some(&node_id) = self
                .name_file_index
                .get(&(name.to_string(), file_path.to_string()))
            {
                return Some(node_id);
            }
        }

        if let Some(node_id) = self
            .name_index
            .get(name)
            .and_then(|candidates| self.select_name_candidate(candidates, file_hint))
        {
            return Some(node_id);
        }

        let name_lower = name.to_lowercase();
        self.name_lower_index
            .get(&name_lower)
            .and_then(|candidates| self.select_name_candidate(candidates, file_hint))
            .or_else(|| self.find_substring_name_match(&name_lower, file_hint))
    }

    fn select_name_candidate(
        &self,
        candidates: &[NodeId],
        file_hint: Option<&str>,
    ) -> Option<NodeId> {
        if let Some(file_path) = file_hint {
            if let Some(node_id) = candidates.iter().copied().find(|node_id| {
                self.get_node(*node_id)
                    .is_some_and(|node| node.file_path.as_ref() == file_path)
            }) {
                return Some(node_id);
            }
        }
        candidates.first().copied()
    }

    fn find_substring_name_match(
        &self,
        name_lower: &str,
        file_hint: Option<&str>,
    ) -> Option<NodeId> {
        match file_hint {
            Some(file_path) => self
                .nodes_in_file(file_path)
                .into_iter()
                .find(|node_id| self.node_contains_name(*node_id, name_lower)),
            None => self
                .graph
                .node_indices()
                .find(|node_id| self.node_contains_name(*node_id, name_lower)),
        }
    }

    fn node_contains_name(&self, node_id: NodeId, name_lower: &str) -> bool {
        self.graph.node_weight(node_id).is_some_and(|node| {
            node.name.to_lowercase().contains(name_lower)
                || node.id.to_lowercase().contains(name_lower)
        })
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

    // -----------------------------------------------------------------------
    // Bulk edge helpers
    // -----------------------------------------------------------------------

    /// Adds multiple call edges to the graph in batch.
    ///
    /// # Arguments
    ///
    /// * `calls` - A vector of (caller, callee) node ID pairs
    pub fn add_call_edges(&mut self, calls: Vec<(NodeId, NodeId)>) {
        for (from, to) in calls {
            self.add_edge(
                from,
                to,
                Edge {
                    edge_type: EdgeType::Call,
                    metadata: EdgeMetadata::empty(),
                },
            );
        }
    }

    /// Adds multiple data flow edges to the graph in batch.
    ///
    /// # Arguments
    ///
    /// * `flows` - A vector of (source, target, variable_name, confidence) tuples
    pub fn add_data_flow_edges(&mut self, flows: Vec<(NodeId, NodeId, String, f32)>) {
        for (from, to, var_name, confidence) in flows {
            self.add_edge(
                from,
                to,
                Edge {
                    edge_type: EdgeType::DataDependency,
                    metadata: EdgeMetadata {
                        call_count: None,
                        variable_name: Some(var_name),
                        confidence: Some(confidence),
                        channel: None,
                        position: None,
                    },
                },
            );
        }
    }

    /// Adds multiple inheritance edges to the graph in batch.
    ///
    /// # Arguments
    ///
    /// * `edges` - A vector of (child, parent, confidence) tuples
    pub fn add_inheritance_edges(&mut self, edges: Vec<(NodeId, NodeId, f32)>) {
        for (child, parent, confidence) in edges {
            self.add_edge(
                child,
                parent,
                Edge {
                    edge_type: EdgeType::Inheritance,
                    metadata: EdgeMetadata::with_confidence(confidence),
                },
            );
        }
    }

    /// Adds multiple containment edges to the graph in batch.
    ///
    /// Containment edges represent structural relationships (e.g., class contains methods)
    /// and should NOT be included in semantic traversals.
    ///
    /// # Arguments
    ///
    /// * `edges` - A vector of (container, contained) node ID pairs
    pub fn add_containment_edges(&mut self, edges: Vec<(NodeId, NodeId)>) {
        for (container, contained) in edges {
            self.add_edge(
                container,
                contained,
                Edge {
                    edge_type: EdgeType::Containment,
                    metadata: EdgeMetadata::empty(),
                },
            );
        }
    }

    /// Adds multiple import edges to the graph in batch.
    ///
    /// # Arguments
    ///
    /// * `imports` - A vector of (importer, imported) node ID pairs
    pub fn add_import_edges(&mut self, imports: Vec<(NodeId, NodeId)>) {
        for (importer, imported) in imports {
            self.add_edge(
                importer,
                imported,
                Edge {
                    edge_type: EdgeType::Import,
                    metadata: EdgeMetadata::empty(),
                },
            );
        }
    }

    // -----------------------------------------------------------------------
    // Embedding accessors
    // -----------------------------------------------------------------------

    /// Stores an embedding for a specific node.
    ///
    /// The embedding is stored in the external `EmbeddingStore`, keeping it
    /// separate from the graph structure to reduce memory usage.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The string identifier of the node (matches `Node.id`)
    /// * `embedding` - The vector embedding (typically 1536 dimensions)
    pub fn set_embedding(&mut self, node_id: &str, embedding: Vec<f32>) {
        self.embedding_store.insert(node_id, embedding);
    }

    /// Retrieves the embedding for a node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The string identifier of the node
    ///
    /// # Returns
    ///
    /// An optional reference to the embedding vector if it exists.
    pub fn get_embedding(&self, node_id: &str) -> Option<&Vec<f32>> {
        self.embedding_store.get(node_id)
    }

    /// Returns the number of embeddings stored in the embedding store.
    pub fn embedding_count(&self) -> usize {
        self.embedding_store.len()
    }

    // -----------------------------------------------------------------------
    // Traversal — all methods require explicit TraversalConfig
    // -----------------------------------------------------------------------

    /// Forward impact: nodes reachable FROM `start` following outgoing edges.
    pub fn forward_impact(&self, start: NodeId, config: &TraversalConfig) -> Vec<NodeId> {
        self.bfs_directed(start, config, Direction::Forward)
    }

    /// Forward impact from many roots using one visited set.
    pub fn forward_impact_multi_source(
        &self,
        starts: &HashSet<NodeId>,
        config: &TraversalConfig,
    ) -> Vec<NodeId> {
        let mut visited = starts.clone();
        let mut ordered_starts = starts.iter().copied().collect::<Vec<_>>();
        ordered_starts.sort_by_key(|id| id.index());
        let mut queue: VecDeque<(NodeId, usize)> =
            ordered_starts.into_iter().map(|id| (id, 0)).collect();
        let mut result = Vec::new();

        while let Some((current, depth)) = queue.pop_front() {
            if let Some(max_nodes) = config.max_nodes {
                if result.len() >= max_nodes {
                    break;
                }
            }
            if !starts.contains(&current)
                && self
                    .graph
                    .node_weight(current)
                    .is_some_and(|node| config.node_should_collect(node))
            {
                result.push(current);
            }
            if config.max_depth.is_some_and(|max_depth| depth >= max_depth) {
                continue;
            }

            let mut scratch = self.bfs_scratch.lock().unwrap();
            scratch.clear();
            scratch.extend(
                self.graph
                    .edges(current)
                    .filter(|edge| config.edge_allowed(edge.weight()))
                    .map(|edge| edge.target()),
            );
            for &neighbor in scratch.iter() {
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }
        result
    }

    /// Backward impact: nodes that can reach `start` following incoming edges.
    pub fn backward_impact(&self, start: NodeId, config: &TraversalConfig) -> Vec<NodeId> {
        self.bfs_directed(start, config, Direction::Backward)
    }

    /// Bidirectional impact: nodes reachable in either direction.
    /// Useful for finding all nodes "related to" a given node.
    pub fn bidirectional_impact(&self, start: NodeId, config: &TraversalConfig) -> Vec<NodeId> {
        let forward = self.bfs_directed(start, config, Direction::Forward);
        let backward = self.bfs_directed(start, config, Direction::Backward);
        let mut combined: HashSet<NodeId> = forward.into_iter().collect();
        combined.extend(backward);
        combined.remove(&start);
        combined.into_iter().collect()
    }

    fn bfs_directed(&self, start: NodeId, config: &TraversalConfig, dir: Direction) -> Vec<NodeId> {
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        let mut result: Vec<NodeId> = Vec::new();

        visited.insert(start);
        queue.push_back((start, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if let Some(max_n) = config.max_nodes {
                if result.len() >= max_n {
                    break;
                }
            }

            if current != start {
                if let Some(node) = self.graph.node_weight(current) {
                    if config.node_should_collect(node) {
                        result.push(current);
                    }
                }
            }

            if let Some(max_d) = config.max_depth {
                if depth >= max_d {
                    continue;
                }
            }

            // Reuse the scratch buffer instead of allocating a new Vec per level.
            let mut scratch = self.bfs_scratch.lock().unwrap();
            scratch.clear();
            match dir {
                Direction::Forward => {
                    // Outgoing edges — filter by edge type
                    scratch.extend(
                        self.graph
                            .edges(current)
                            .filter(|e| config.edge_allowed(e.weight()))
                            .map(|e| e.target()),
                    );
                }
                Direction::Backward => {
                    use petgraph::Direction as PD;
                    scratch.extend(
                        self.graph
                            .edges_directed(current, PD::Incoming)
                            .filter(|e| config.edge_allowed(e.weight()))
                            .map(|e| e.source()),
                    );
                }
            }

            for &neighbor in scratch.iter() {
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, depth + 1));
                }
            }
            // scratch (MutexGuard) dropped here → lock released
        }

        result
    }

    // -----------------------------------------------------------------------
    // Serialization
    // -----------------------------------------------------------------------

    /// Serializes the PDG to a binary format.
    ///
    /// Uses bincode for efficient serialization. The serialized format includes
    /// all nodes, edges, and indexes.
    ///
    /// # Returns
    ///
    /// A Result containing the serialized bytes, or an error message if serialization fails.
    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(&SerializablePDG::from_pdg(self))
            .map_err(|e| format!("Serialize failed: {}", e))
    }

    /// Deserializes a PDG from binary data.
    ///
    /// Restores a ProgramDependenceGraph from bytes previously serialized with `serialize()`.
    ///
    /// # Arguments
    ///
    /// * `data` - The binary data to deserialize
    ///
    /// # Returns
    ///
    /// A Result containing the deserialized PDG, or an error message if deserialization fails.
    pub fn deserialize(data: &[u8]) -> Result<Self, String> {
        bincode::deserialize::<SerializablePDG>(data)
            .map_err(|e| format!("Deserialize failed: {}", e))
            .and_then(|s| s.to_pdg())
    }

    // Legacy API aliases for backward compatibility during migration

    /// Gets nodes reachable from the given node (forward impact).
    ///
    /// # Deprecated
    ///
    /// Since 2.0.0: Use `forward_impact` with `TraversalConfig` instead.
    /// This method uses a default configuration that may not be appropriate
    /// for all use cases.
    #[deprecated(
        since = "2.0.0",
        note = "Use forward_impact with TraversalConfig instead"
    )]
    pub fn get_forward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        self.forward_impact(node_id, &TraversalConfig::for_impact_analysis())
    }

    /// Gets nodes that can reach the given node (backward impact).
    ///
    /// # Deprecated
    ///
    /// Since 2.0.0: Use `backward_impact` with `TraversalConfig` instead.
    /// This method uses a default configuration that may not be appropriate
    /// for all use cases.
    #[deprecated(
        since = "2.0.0",
        note = "Use backward_impact with TraversalConfig instead"
    )]
    pub fn get_backward_impact(&self, node_id: NodeId) -> Vec<NodeId> {
        self.backward_impact(node_id, &TraversalConfig::for_impact_analysis())
    }

    /// Gets nodes reachable from the given node with a depth bound.
    ///
    /// # Deprecated
    ///
    /// Since 2.0.0: Use `forward_impact` with `TraversalConfig` instead.
    /// The `TraversalConfig` provides more flexible control over traversal
    /// bounds and filtering.
    #[deprecated(
        since = "2.0.0",
        note = "Use forward_impact with TraversalConfig instead"
    )]
    pub fn get_forward_impact_bounded(&self, start: NodeId, max_depth: usize) -> Vec<NodeId> {
        let config = TraversalConfig {
            max_depth: Some(max_depth),
            max_nodes: Some(500),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
            ]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.0,
        };
        self.forward_impact(start, &config)
    }

    /// Gets nodes that can reach the given node with a depth bound.
    ///
    /// # Deprecated
    ///
    /// Since 2.0.0: Use `backward_impact` with `TraversalConfig` instead.
    /// The `TraversalConfig` provides more flexible control over traversal
    /// bounds and filtering.
    #[deprecated(
        since = "2.0.0",
        note = "Use backward_impact with TraversalConfig instead"
    )]
    pub fn get_backward_impact_bounded(&self, start: NodeId, max_depth: usize) -> Vec<NodeId> {
        let config = TraversalConfig {
            max_depth: Some(max_depth),
            max_nodes: Some(500),
            allowed_edge_types: Some(&[
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
            ]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.0,
        };
        self.backward_impact(start, &config)
    }

    /// Adds call graph edges (legacy alias - use add_call_edges).
    ///
    /// # Deprecated
    ///
    /// This method is provided for backward compatibility. New code should use
    /// `add_call_edges` instead.
    ///
    /// # Arguments
    ///
    /// * `calls` - A vector of (caller, callee) node ID pairs
    pub fn add_call_graph_edges(&mut self, calls: Vec<(NodeId, NodeId)>) {
        self.add_call_edges(calls);
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
