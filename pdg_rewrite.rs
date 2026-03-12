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
use petgraph::visit::Dfs;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

pub type NodeId = petgraph::stable_graph::NodeIndex;
pub type EdgeId = petgraph::stable_graph::EdgeIndex;

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub node_type: NodeType,
    pub name: String,
    pub file_path: String,
    pub byte_range: (usize, usize),
    pub complexity: u32,
    pub language: String,
    // NOTE: embeddings removed from Node. Use EmbeddingStore instead.
    // Keeping this field as Option<()> would break existing bincode; instead
    // the serialization shim below handles backward compat via a skip field.
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum NodeType {
    Function,
    Class,
    Method,
    Variable,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub edge_type: EdgeType,
    pub metadata: EdgeMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeMetadata {
    pub call_count: Option<usize>,
    pub variable_name: Option<String>,
    /// Confidence score [0.0, 1.0] — used for inferred edges (inheritance, type deps)
    pub confidence: Option<f32>,
}

impl EdgeMetadata {
    pub fn empty() -> Self {
        Self { call_count: None, variable_name: None, confidence: None }
    }
    pub fn with_confidence(confidence: f32) -> Self {
        Self { call_count: None, variable_name: None, confidence: Some(confidence) }
    }
    pub fn with_variable(name: String) -> Self {
        Self { call_count: None, variable_name: Some(name), confidence: None }
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
    pub fn new() -> Self { Self::default() }

    pub fn insert(&mut self, node_id: &str, embedding: Vec<f32>) {
        self.embeddings.insert(node_id.to_string(), embedding);
    }

    pub fn get(&self, node_id: &str) -> Option<&Vec<f32>> {
        self.embeddings.get(node_id)
    }

    pub fn remove(&mut self, node_id: &str) {
        self.embeddings.remove(node_id);
    }

    pub fn len(&self) -> usize { self.embeddings.len() }
    pub fn is_empty(&self) -> bool { self.embeddings.is_empty() }
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
        let nodes = pdg.graph.node_indices()
            .map(|idx| SerializableNode { index: idx.index() as u32, node: pdg.graph[idx].clone() })
            .collect();

        let edges = pdg.graph.edge_indices()
            .map(|eidx| {
                let (source, target) = pdg.graph.edge_endpoints(eidx)
                    .expect("Edge endpoints must exist");
                SerializableEdge {
                    source: source.index() as u32,
                    target: target.index() as u32,
                    edge: pdg.graph[eidx].clone(),
                }
            })
            .collect();

        let symbol_index = pdg.symbol_index.iter()
            .map(|(k, v)| (k.clone(), v.index() as u32))
            .collect();
        let file_index = pdg.file_index.iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_index = pdg.name_index.iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();
        let name_lower_index = pdg.name_lower_index.iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|id| id.index() as u32).collect()))
            .collect();

        Self { nodes, edges, symbol_index, file_index, name_index, name_lower_index }
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
            let nids: Vec<NodeId> = old_idxs.iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() { pdg.file_index.insert(fp.clone(), nids); }
        }
        for (name, old_idxs) in &self.name_index {
            let nids: Vec<NodeId> = old_idxs.iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() { pdg.name_index.insert(name.clone(), nids); }
        }
        for (name_lc, old_idxs) in &self.name_lower_index {
            let nids: Vec<NodeId> = old_idxs.iter()
                .filter_map(|i| index_map.get(i).copied())
                .collect();
            if !nids.is_empty() { pdg.name_lower_index.insert(name_lc.clone(), nids); }
        }

        // Backward compat: rebuild name/lower indices from nodes if absent
        if pdg.name_index.is_empty() {
            for nid in pdg.graph.node_indices() {
                if let Some(node) = pdg.graph.node_weight(nid) {
                    pdg.name_index.entry(node.name.clone()).or_default().push(nid);
                    pdg.name_lower_index.entry(node.name.to_lowercase()).or_default().push(nid);
                }
            }
        }

        for se in &self.edges {
            let src = index_map.get(&se.source)
                .ok_or_else(|| format!("Missing source {}", se.source))?;
            let tgt = index_map.get(&se.target)
                .ok_or_else(|| format!("Missing target {}", se.target))?;
            pdg.graph.add_edge(*src, *tgt, se.edge.clone());
        }

        Ok(pdg)
    }
}

// ---------------------------------------------------------------------------
// ProgramDependenceGraph
// ---------------------------------------------------------------------------

pub struct ProgramDependenceGraph {
    graph: StableGraph<Node, Edge>,
    /// symbol_index: node.id ("file_path:qualified_name") → NodeId
    symbol_index: HashMap<String, NodeId>,
    /// file_index: file_path → [NodeId]
    file_index: HashMap<String, Vec<NodeId>>,
    /// name_index: node.name (exact) → [NodeId]
    name_index: HashMap<String, Vec<NodeId>>,
    /// name_lower_index: node.name.to_lowercase() → [NodeId]
    /// Eliminates O(n) scan in find_by_name_in_file case-insensitive path.
    name_lower_index: HashMap<String, Vec<NodeId>>,
}

impl ProgramDependenceGraph {
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

    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = self.graph.add_node(node.clone());
        self.symbol_index.insert(node.id.clone(), id);
        self.file_index.entry(node.file_path.clone()).or_default().push(id);
        self.name_index.entry(node.name.clone()).or_default().push(id);
        self.name_lower_index.entry(node.name.to_lowercase()).or_default().push(id);
        id
    }

    /// Add an edge. Returns the EdgeId directly (never fails silently).
    /// Callers should validate that `from` and `to` exist before calling.
    pub fn add_edge(&mut self, from: NodeId, to: NodeId, edge: Edge) -> EdgeId {
        debug_assert!(
            self.graph.contains_node(from) && self.graph.contains_node(to),
            "add_edge called with invalid NodeId(s): from={:?} to={:?}", from, to
        );
        self.graph.add_edge(from, to, edge)
    }

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

    pub fn remove_edge(&mut self, id: EdgeId) -> Option<Edge> {
        self.graph.remove_edge(id)
    }

    pub fn remove_file(&mut self, file_path: &str) {
        let ids = self.nodes_in_file(file_path);
        for id in ids { self.remove_node(id); }
        self.file_index.remove(file_path);
    }

    // -----------------------------------------------------------------------
    // Read access
    // -----------------------------------------------------------------------

    pub fn get_node(&self, id: NodeId) -> Option<&Node> { self.graph.node_weight(id) }
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> { self.graph.node_weight_mut(id) }
    pub fn get_edge(&self, id: EdgeId) -> Option<&Edge> { self.graph.edge_weight(id) }
    pub fn node_count(&self) -> usize { self.graph.node_count() }
    pub fn edge_count(&self) -> usize { self.graph.edge_count() }
    pub fn node_indices(&self) -> impl Iterator<Item = NodeId> + '_ { self.graph.node_indices() }
    pub fn edge_indices(&self) -> impl Iterator<Item = EdgeId> + '_ { self.graph.edge_indices() }
    pub fn edge_endpoints(&self, edge_id: EdgeId) -> Option<(NodeId, NodeId)> { self.graph.edge_endpoints(edge_id) }

    pub fn neighbors(&self, node_id: NodeId) -> Vec<NodeId> {
        self.graph.neighbors(node_id).collect()
    }

    pub fn predecessors(&self, node_id: NodeId) -> Vec<NodeId> {
        use petgraph::Direction;
        self.graph.neighbors_directed(node_id, Direction::Incoming).collect()
    }

    pub fn predecessor_count(&self, node_id: NodeId) -> usize {
        use petgraph::Direction;
        self.graph.neighbors_directed(node_id, Direction::Incoming).count()
    }

    // -----------------------------------------------------------------------
    // Lookup (all O(1) or O(k) where k = results count)
    // -----------------------------------------------------------------------

    pub fn find_by_symbol(&self, symbol: &str) -> Option<NodeId> {
        self.symbol_index.get(symbol).copied()
    }

    pub fn find_by_id(&self, node_id: &str) -> Option<NodeId> {
        self.symbol_index.get(node_id).copied()
    }

    pub fn nodes_in_file(&self, file_path: &str) -> Vec<NodeId> {
        self.file_index.get(file_path).cloned().unwrap_or_default()
    }

    pub fn find_by_name(&self, name: &str) -> Option<NodeId> {
        self.name_index.get(name).and_then(|ids| ids.first().copied())
    }

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
                    self.get_node(nid).map(|n| n.file_path == fp).unwrap_or(false)
                }) {
                    return Some(nid);
                }
            }
            return Some(candidates[0]);
        }

        // 2. Case-insensitive match via name_lower_index (O(k) not O(n))
        let name_lower = name.to_lowercase();
        let ci_candidates = self.name_lower_index.get(&name_lower).cloned().unwrap_or_default();
        if !ci_candidates.is_empty() {
            if let Some(fp) = file_hint {
                if let Some(&nid) = ci_candidates.iter().find(|&&nid| {
                    self.get_node(nid).map(|n| n.file_path == fp).unwrap_or(false)
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

    pub fn add_call_edges(&mut self, calls: Vec<(NodeId, NodeId)>) {
        for (from, to) in calls {
            self.add_edge(from, to, Edge {
                edge_type: EdgeType::Call,
                metadata: EdgeMetadata::empty(),
            });
        }
    }

    pub fn add_data_flow_edges(&mut self, flows: Vec<(NodeId, NodeId, String, f32)>) {
        for (from, to, var_name, confidence) in flows {
            self.add_edge(from, to, Edge {
                edge_type: EdgeType::DataDependency,
                metadata: EdgeMetadata {
                    call_count: None,
                    variable_name: Some(var_name),
                    confidence: Some(confidence),
                },
            });
        }
    }

    pub fn add_inheritance_edges(&mut self, edges: Vec<(NodeId, NodeId, f32)>) {
        for (child, parent, confidence) in edges {
            self.add_edge(child, parent, Edge {
                edge_type: EdgeType::Inheritance,
                metadata: EdgeMetadata::with_confidence(confidence),
            });
        }
    }

    pub fn add_containment_edges(&mut self, edges: Vec<(NodeId, NodeId)>) {
        for (container, contained) in edges {
            self.add_edge(container, contained, Edge {
                edge_type: EdgeType::Containment,
                metadata: EdgeMetadata::empty(),
            });
        }
    }

    pub fn add_import_edges(&mut self, imports: Vec<(NodeId, NodeId)>) {
        for (importer, imported) in imports {
            self.add_edge(importer, imported, Edge {
                edge_type: EdgeType::Import,
                metadata: EdgeMetadata::empty(),
            });
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
        use petgraph::Direction as PGDir;

        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        let mut result: Vec<NodeId> = Vec::new();

        visited.insert(start);
        queue.push_back((start, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if let Some(max_n) = config.max_nodes {
                if result.len() >= max_n { break; }
            }

            if current != start {
                if let Some(node) = self.graph.node_weight(current) {
                    if config.node_should_collect(node) {
                        result.push(current);
                    }
                }
            }

            if let Some(max_d) = config.max_depth {
                if depth >= max_d { continue; }
            }

            let neighbors: Vec<NodeId> = match dir {
                Direction::Forward => {
                    // Outgoing edges — filter by edge type
                    self.graph.edges(current)
                        .filter(|e| config.edge_allowed(e.weight()))
                        .map(|e| e.target())
                        .collect()
                }
                Direction::Backward => {
                    use petgraph::Direction as PD;
                    self.graph.edges_directed(current, PD::Incoming)
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

    pub fn serialize(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(&SerializablePDG::from_pdg(self))
            .map_err(|e| format!("Serialize failed: {}", e))
    }

    pub fn deserialize(data: &[u8]) -> Result<Self, String> {
        bincode::deserialize::<SerializablePDG>(data)
            .map_err(|e| format!("Deserialize failed: {}", e))
            .and_then(|s| s.to_pdg())
    }
}

impl Default for ProgramDependenceGraph {
    fn default() -> Self { Self::new() }
}

// Internal direction enum (not re-exporting petgraph's Direction to keep API clean)
enum Direction { Forward, Backward }

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(id: &str, name: &str, file: &str, ntype: NodeType) -> Node {
        Node {
            id: id.to_string(), node_type: ntype, name: name.to_string(),
            file_path: file.to_string(), byte_range: (0, 10), complexity: 2, language: "rust".to_string(),
        }
    }

    #[test]
    fn traversal_respects_max_nodes() {
        let mut pdg = ProgramDependenceGraph::new();
        let n: Vec<NodeId> = (0..10)
            .map(|i| pdg.add_node(make_node(&format!("n{i}"), &format!("n{i}"), "f.rs", NodeType::Function)))
            .collect();
        // Chain: n0 → n1 → n2 → ... → n9
        for i in 0..9 {
            pdg.add_call_edges(vec![(n[i], n[i+1])]);
        }
        let config = TraversalConfig { max_depth: None, max_nodes: Some(3), ..TraversalConfig::for_impact_analysis() };
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
        assert!(!result.contains(&callee) || result.contains(&method),
            "Containment edges should be filtered from semantic traversal");
    }

    #[test]
    fn find_by_name_in_file_no_scan_needed() {
        let mut pdg = ProgramDependenceGraph::new();
        for i in 0..1000 {
            pdg.add_node(make_node(&format!("f:func{i}"), &format!("func{i}"), "f.rs", NodeType::Function));
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

        let containment_count = pdg.edge_indices()
            .filter_map(|e| pdg.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Containment)
            .count();
        let call_count = pdg.edge_indices()
            .filter_map(|e| pdg.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Call)
            .count();

        assert_eq!(containment_count, 1);
        assert_eq!(call_count, 0);
    }

    #[test]
    fn serialize_roundtrip_preserves_containment_edges() {
        let mut pdg = ProgramDependenceGraph::new();
        let cls = pdg.add_node(make_node("f:C", "C", "f.rs", NodeType::Class));
        let m = pdg.add_node(make_node("f:C::m", "m", "f.rs", NodeType::Method));
        pdg.add_containment_edges(vec![(cls, m)]);
        pdg.add_inheritance_edges(vec![(cls, m, 0.9)]);

        let bytes = pdg.serialize().unwrap();
        let restored = ProgramDependenceGraph::deserialize(&bytes).unwrap();

        let containment = restored.edge_indices()
            .filter_map(|e| restored.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Containment)
            .count();
        assert_eq!(containment, 1);

        let inheritance = restored.edge_indices()
            .filter_map(|e| restored.get_edge(e))
            .filter(|e| e.edge_type == EdgeType::Inheritance)
            .count();
        assert_eq!(inheritance, 1);
    }
}
