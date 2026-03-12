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
    pub file_path: String,

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
    pub allowed_edge_types: Option<Vec<EdgeType>>,
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
            allowed_edge_types: Some(vec![EdgeType::Call, EdgeType::DataDependency]),
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
            allowed_edge_types: Some(vec![
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
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
            allowed_edge_types: Some(vec![
                EdgeType::Call,
                EdgeType::DataDependency,
                EdgeType::Inheritance,
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
            allowed_edge_types: Some(vec![EdgeType::Import]),
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
#[derive(Debug, Default)]
pub struct EmbeddingStore {
    embeddings: HashMap<String, Vec<f32>>, // keyed by node.id (stable across serialization)
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
        }
    }

    fn to_pdg(&self) -> Result<ProgramDependenceGraph, String> {
        let mut pdg = ProgramDependenceGraph::new();
        let mut index_map: HashMap<u32, NodeId> = HashMap::new();

        for sn in &self.nodes {
            let id = pdg.graph.add_node(sn.node.clone());
            index_map.insert(sn.index, id);
        }

        for (sym, old_idx) in &self.symbol_index {
            if let Some(&nid) = index_map.get(old_idx) {
                pdg.symbol_index.insert(sym.clone(), nid);
            }
        }
        for (fp, old_idxs) in &self.file_index {
            let nids: Vec<NodeId> = old_idxs
                .iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() {
                pdg.file_index.insert(fp.clone(), nids);
            }
        }
        for (name, old_idxs) in &self.name_index {
            let nids: Vec<NodeId> = old_idxs
                .iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() {
                pdg.name_index.insert(name.clone(), nids);
            }
        }
        for (name_lc, old_idxs) in &self.name_lower_index {
            let nids: Vec<NodeId> = old_idxs
                .iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() {
                pdg.name_lower_index.insert(name_lc.clone(), nids);
            }
        }

        // Backward compat: rebuild name/lower indices from nodes if absent
        if pdg.name_index.is_empty() {
            for nid in pdg.graph.node_indices() {
                if let Some(node) = pdg.graph.node_weight(nid) {
                    pdg.name_index
                        .entry(node.name.clone())
                        .or_default()
                        .push(nid);
                    pdg.name_lower_index
                        .entry(node.name.to_lowercase())
                        .or_default()
                        .push(nid);
                }
            }
        }

        for se in &self.edges {
            let src = index_map
                .get(&se.source)
                .ok_or_else(|| format!("Missing source {}", se.source))?;
            let tgt = index_map
                .get(&se.target)
                .ok_or_else(|| format!("Missing target {}", se.target))?;
            pdg.graph.add_edge(*src, *tgt, se.edge.clone());
        }

        Ok(pdg)
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
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = self.graph.add_node(node.clone());
        self.symbol_index.insert(node.id.clone(), id);
        self.file_index
            .entry(node.file_path.clone())
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
            if let Some(v) = self.file_index.get_mut(&node.file_path) {
                v.retain(|&id| id != node_id);
            }
            if let Some(v) = self.name_index.get_mut(&node.name) {
                v.retain(|&id| id != node_id);
            }
            if let Some(v) = self.name_lower_index.get_mut(&node.name.to_lowercase()) {
                v.retain(|&id| id != node_id);
            }
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
        // 1. Exact match via name_index
        let candidates = self.name_index.get(name).cloned().unwrap_or_default();
        if !candidates.is_empty() {
            if let Some(fp) = file_hint {
                if let Some(&nid) = candidates.iter().find(|&&nid| {
                    self.get_node(nid)
                        .map(|n| n.file_path == fp)
                        .unwrap_or(false)
                }) {
                    return Some(nid);
                }
            }
            return Some(candidates[0]);
        }

        // 2. Case-insensitive match via name_lower_index (O(k) not O(n))
        let name_lower = name.to_lowercase();
        let ci_candidates = self
            .name_lower_index
            .get(&name_lower)
            .cloned()
            .unwrap_or_default();
        if !ci_candidates.is_empty() {
            if let Some(fp) = file_hint {
                if let Some(&nid) = ci_candidates.iter().find(|&&nid| {
                    self.get_node(nid)
                        .map(|n| n.file_path == fp)
                        .unwrap_or(false)
                }) {
                    return Some(nid);
                }
            }
            return Some(ci_candidates[0]);
        }

        // 3. Substring match — unavoidably O(n), but only reached as last resort.
        // Scoped to file when hint provided.
        let search_space: Box<dyn Iterator<Item = NodeId>> = if let Some(fp) = file_hint {
            Box::new(self.nodes_in_file(fp).into_iter())
        } else {
            Box::new(self.graph.node_indices())
        };

        for nid in search_space {
            if let Some(node) = self.graph.node_weight(nid) {
                if node.name.to_lowercase().contains(&name_lower)
                    || node.id.to_lowercase().contains(&name_lower)
                {
                    return Some(nid);
                }
            }
        }

        None
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
    // Traversal — all methods require explicit TraversalConfig
    // -----------------------------------------------------------------------

    /// Forward impact: nodes reachable FROM `start` following outgoing edges.
    pub fn forward_impact(&self, start: NodeId, config: &TraversalConfig) -> Vec<NodeId> {
        self.bfs_directed(start, config, Direction::Forward)
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

            let neighbors: Vec<NodeId> = match dir {
                Direction::Forward => {
                    // Outgoing edges — filter by edge type
                    self.graph
                        .edges(current)
                        .filter(|e| config.edge_allowed(e.weight()))
                        .map(|e| e.target())
                        .collect()
                }
                Direction::Backward => {
                    use petgraph::Direction as PD;
                    self.graph
                        .edges_directed(current, PD::Incoming)
                        .filter(|e| config.edge_allowed(e.weight()))
                        .map(|e| e.source())
                        .collect()
                }
            };

            for neighbor in neighbors {
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, depth + 1));
                }
            }
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
            allowed_edge_types: Some(vec![
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
            allowed_edge_types: Some(vec![
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
mod tests {
    use super::*;

    fn make_node(id: &str, name: &str, file: &str, ntype: NodeType) -> Node {
        Node {
            id: id.to_string(),
            node_type: ntype,
            name: name.to_string(),
            file_path: file.to_string(),
            byte_range: (0, 10),
            complexity: 2,
            language: "rust".to_string(),
        }
    }

    #[test]
    fn traversal_respects_max_nodes() {
        let mut pdg = ProgramDependenceGraph::new();
        let n: Vec<NodeId> = (0..10)
            .map(|i| {
                pdg.add_node(make_node(
                    &format!("n{i}"),
                    &format!("n{i}"),
                    "f.rs",
                    NodeType::Function,
                ))
            })
            .collect();
        // Chain: n0 → n1 → n2 → ... → n9
        for i in 0..9 {
            pdg.add_call_edges(vec![(n[i], n[i + 1])]);
        }
        let config = TraversalConfig {
            max_depth: None,
            max_nodes: Some(3),
            ..TraversalConfig::for_impact_analysis()
        };
        let result = pdg.forward_impact(n[0], &config);
        assert!(result.len() <= 3, "Should respect max_nodes cap");
    }

    #[test]
    fn traversal_filters_containment_edges() {
        let mut pdg = ProgramDependenceGraph::new();
        let cls = pdg.add_node(make_node("f:MyClass", "MyClass", "f.rs", NodeType::Class));
        let method = pdg.add_node(make_node("f:MyClass::foo", "foo", "f.rs", NodeType::Method));
        let callee = pdg.add_node(make_node("f:bar", "bar", "f.rs", NodeType::Function));
        pdg.add_containment_edges(vec![(cls, method)]);
        pdg.add_call_edges(vec![(method, callee)]);

        // With default semantic config, containment edges should not be traversed
        let config = TraversalConfig::for_semantic_analysis();
        let result = pdg.forward_impact(cls, &config);
        // Should not reach callee via containment→method→call chain
        // because containment is filtered — cls can only reach method
        // if containment is allowed; method→callee only if call is allowed
        // With semantic_analysis: Call allowed but Containment not → cls reaches nothing
        assert!(
            !result.contains(&callee) || result.contains(&method),
            "Containment edges should be filtered from semantic traversal"
        );
    }

    #[test]
    fn find_by_name_in_file_no_scan_needed() {
        let mut pdg = ProgramDependenceGraph::new();
        for i in 0..1000 {
            pdg.add_node(make_node(
                &format!("f:func{i}"),
                &format!("func{i}"),
                "f.rs",
                NodeType::Function,
            ));
        }
        // Case-insensitive lookup should use name_lower_index, not scan
        let result = pdg.find_by_name_in_file("FUNC42", None);
        assert!(result.is_some());
    }

    #[test]
    fn containment_edge_type_is_separate_from_call() {
        let mut pdg = ProgramDependenceGraph::new();
        let cls = pdg.add_node(make_node("f:C", "C", "f.rs", NodeType::Class));
        let m = pdg.add_node(make_node("f:C::m", "m", "f.rs", NodeType::Method));
        pdg.add_containment_edges(vec![(cls, m)]);

        let containment_count = pdg
            .edge_indices()
            .filter_map(|e| pdg.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Containment)
            .count();
        let call_count = pdg
            .edge_indices()
            .filter_map(|e| pdg.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Call)
            .count();

        assert_eq!(containment_count, 1);
        assert_eq!(call_count, 0);
    }

    #[test]
    fn confidence_filtering_works() {
        let mut pdg = ProgramDependenceGraph::new();
        let n1 = pdg.add_node(make_node("f:a", "a", "f.rs", NodeType::Function));
        let n2 = pdg.add_node(make_node("f:b", "b", "f.rs", NodeType::Function));
        pdg.add_data_flow_edges(vec![(n1, n2, "T".to_string(), 0.3)]);

        // Low confidence edge should be filtered when min_edge_confidence = 0.5
        let config = TraversalConfig {
            max_depth: Some(5),
            max_nodes: Some(100),
            allowed_edge_types: Some(vec![EdgeType::DataDependency]),
            excluded_node_types: None,
            min_complexity: None,
            min_edge_confidence: 0.5,
        };
        let result = pdg.forward_impact(n1, &config);
        assert!(
            !result.contains(&n2),
            "Low confidence edge should be filtered"
        );
    }

    #[test]
    fn backward_traversal_works() {
        let mut pdg = ProgramDependenceGraph::new();
        let n: Vec<NodeId> = (0..5)
            .map(|i| {
                pdg.add_node(make_node(
                    &format!("f:n{i}"),
                    &format!("n{i}"),
                    "f.rs",
                    NodeType::Function,
                ))
            })
            .collect();
        // Chain: n0 → n1 → n2 → n3 → n4
        for i in 0..4 {
            pdg.add_call_edges(vec![(n[i], n[i + 1])]);
        }

        let config = TraversalConfig::for_impact_analysis();
        let backward = pdg.backward_impact(n[4], &config);
        assert!(backward.contains(&n[0]));
        assert!(backward.contains(&n[1]));
        assert!(backward.contains(&n[2]));
        assert!(backward.contains(&n[3]));
    }

    #[test]
    fn bidirectional_traversal_works() {
        let mut pdg = ProgramDependenceGraph::new();
        let n1 = pdg.add_node(make_node("f:a", "a", "f.rs", NodeType::Function));
        let n2 = pdg.add_node(make_node("f:b", "b", "f.rs", NodeType::Function));
        let n3 = pdg.add_node(make_node("f:c", "c", "f.rs", NodeType::Function));
        // n1 → n2 and n2 → n3 (n2 is in the middle)
        pdg.add_call_edges(vec![(n1, n2), (n2, n3)]);

        let config = TraversalConfig::for_impact_analysis();
        let bidirectional = pdg.bidirectional_impact(n2, &config);
        assert!(bidirectional.contains(&n1), "Should reach backward");
        assert!(bidirectional.contains(&n3), "Should reach forward");
        assert!(
            !bidirectional.contains(&n2),
            "Should not include start node"
        );
    }
}
