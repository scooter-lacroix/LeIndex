//! Lossless PDG1 v2 codec between the in-memory graph and generation layers.
//!
//! Encode direction: a resident [`ProgramDependenceGraph`] becomes the
//! version-2 [`LIDX-PDG1`] payload — the same format
//! [`migrate::encode_pdg_layer_v2`](super::migrate::encode_pdg_layer_v2)
//! synthesizes from a legacy catalog, so generation layers produced by
//! migration and by live publication are interchangeable. Decode direction:
//! a [`PdgReader`] over a version-2 payload rebuilds the full graph
//! (nodes with every field, edges with metadata, precision markers,
//! trigram index) without touching the SQL catalog.
//!
//! Edge endpoints reference nodes by their **interned `node.id` string id**
//! (not a positional row index) — the convention shared with the migration
//! encoder and asserted by `migrate_test`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::graph::pdg::{Edge, EdgeMetadata, EdgeType, Node, NodeType, ProgramDependenceGraph};
use crate::storage::generation::migrate::{
    MigrationError, StringInterner, edge_type_code_v2, edge_type_name_v2, node_type_code_v2,
    node_type_name_v2,
};

/// The canonical empty TF-IDF payload, re-exported for integration tests and
/// external publishers that stage all five layers.
pub fn encode_empty_tfidf() -> Vec<u8> {
    crate::storage::generation::migrate::encode_empty_tfidf()
}

/// The canonical empty neural payload, re-exported for the same callers.
pub fn encode_empty_neural() -> Vec<u8> {
    crate::storage::generation::migrate::encode_empty_neural()
}

/// VACUUM-normalized catalog bytes (the DB layer payload), re-exported for
/// the same callers.
pub fn vacuum_catalog_bytes(db_path: &std::path::Path) -> Result<Vec<u8>, MigrationError> {
    crate::storage::generation::migrate::vacuum_bytes(db_path)
}
use crate::storage::generation::reader::{
    PDG_EDGE_LEN, PDG_EDGE_META_LEN, PDG_HEADER_LEN, PDG_MAGIC, PDG_NODE_V2_LEN,
    PDG_STRING_OFFSET_LEN, PDG_V2_NONE, PdgReader, ReaderError,
};
use crate::storage::nodes::NodeType as StorageNodeType;

/// Errors encountered while encoding or decoding a PDG layer.
#[derive(Debug, thiserror::Error)]
pub enum GraphCodecError {
    /// A migration-format encoder rejected data that cannot fit losslessly.
    #[error("PDG layer payload error: {0}")]
    Payload(#[from] MigrationError),
    /// The mapped PDG layer reader rejected the payload.
    #[error("PDG layer reader error: {0}")]
    Reader(#[from] ReaderError),
    /// A layer field was invalid or could not be mapped to graph vocabulary.
    #[error("invalid PDG layer: {0}")]
    Invalid(String),
}

/// The stable u32 node-id assignment a layer payload uses for its records.
///
/// Ids are 0-based positions into the node list ordered lexicographically by
/// `node.id` — deterministic across runs and independent of petgraph's
/// insertion order. The Tfidf/Neural layer encoders consume the same map so
/// every layer in one generation agrees on which u32 is which node.
#[derive(Debug, Clone)]
pub struct LayerNodeAssignment {
    node_ids: Vec<String>,
    index_by_id: HashMap<String, u32>,
}

impl LayerNodeAssignment {
    /// Stable u32 id of `node_id` (position in the sorted node list).
    pub fn get(&self, node_id: &str) -> Option<u32> {
        self.index_by_id.get(node_id).copied()
    }

    /// The full `node.id -> u32` map, keyed the way the vector-layer encoders
    /// consume it (they look rows up by the graph's string node id).
    pub fn as_map(&self) -> HashMap<String, u32> {
        self.index_by_id.clone()
    }

    /// Number of assigned nodes.
    pub fn len(&self) -> usize {
        self.node_ids.len()
    }

    /// Whether no node was assigned (empty graph).
    pub fn is_empty(&self) -> bool {
        self.node_ids.is_empty()
    }

    /// Sorted node ids in assignment order.
    pub fn node_ids(&self) -> &[String] {
        &self.node_ids
    }
}

/// Encode a graph into the lossless v2 PDG payload used by the CAS layer.
///
/// Duplicate stable node IDs collapse to their last graph record (matching
/// the legacy `(project_id, node_id)` SQL upsert semantics), nodes are
/// written in ascending `node.id` order, and edges reference endpoints by
/// interned node-id string id — byte-for-byte the format the migration
/// encoder produces for the same graph.
pub fn encode_pdg_v2_from_graph(
    pdg: &ProgramDependenceGraph,
) -> Result<(Vec<u8>, LayerNodeAssignment), GraphCodecError> {
    // Last-wins map over duplicate stable ids, then a deterministic order.
    let mut nodes_by_id: HashMap<&str, &Node> = HashMap::with_capacity(pdg.node_count());
    for index in pdg.node_indices() {
        let node = pdg
            .get_node(index)
            .ok_or_else(|| GraphCodecError::Invalid("node index has no node".into()))?;
        nodes_by_id.insert(node.id.as_str(), node);
    }
    let node_ids: Vec<String> = {
        let mut ids: Vec<&str> = nodes_by_id.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().map(str::to_owned).collect()
    };

    let mut interner = StringInterner::new();
    let mut node_records = Vec::with_capacity(node_ids.len() * PDG_NODE_V2_LEN);
    for id in &node_ids {
        let node = nodes_by_id[id.as_str()];
        let node_type = storage_node_type(&node.node_type);
        node_records.extend_from_slice(&interner.intern(&node.id).to_le_bytes());
        node_records.extend_from_slice(&interner.intern(&node.name).to_le_bytes());
        node_records.extend_from_slice(&interner.intern(&node.file_path).to_le_bytes());
        node_records.extend_from_slice(&interner.intern(&node.language).to_le_bytes());
        node_records.extend_from_slice(&node_type_code_v2(node_type.as_str())?.to_le_bytes());
        node_records.extend_from_slice(&node.complexity.to_le_bytes());
        node_records.extend_from_slice(
            &u32::try_from(node.byte_range.0)
                .map_err(|_| GraphCodecError::Invalid("node byte start exceeds u32".into()))?
                .to_le_bytes(),
        );
        node_records.extend_from_slice(
            &u32::try_from(node.byte_range.1)
                .map_err(|_| GraphCodecError::Invalid("node byte end exceeds u32".into()))?
                .to_le_bytes(),
        );
        node_records.extend_from_slice(&u32::from(pdg.is_precision_symbol(&node.id)).to_le_bytes());
    }

    // Resolve edge endpoints to interned node-id ids and collapse parallel
    // edges on (source, target, type) — the legacy `intel_edges` PRIMARY KEY
    // dedups them with last-write-wins on metadata, so the layer must too or
    // a layer-hydrated graph drifts from SQL-hydration semantics. `intern` is
    // idempotent, so re-interning each endpoint's id recovers its assignment.
    let mut edges_by_key: HashMap<(u32, u32, u32), Vec<u8>> =
        HashMap::with_capacity(pdg.edge_count());
    for edge_id in pdg.edge_indices() {
        let edge = pdg
            .get_edge(edge_id)
            .ok_or_else(|| GraphCodecError::Invalid("edge index has no edge".into()))?;
        let (source, target) = pdg
            .edge_endpoints(edge_id)
            .ok_or_else(|| GraphCodecError::Invalid("edge has no endpoints".into()))?;
        let source = pdg
            .get_node(source)
            .ok_or_else(|| GraphCodecError::Invalid("edge source node is missing".into()))?;
        let target = pdg
            .get_node(target)
            .ok_or_else(|| GraphCodecError::Invalid("edge target node is missing".into()))?;
        // A duplicate-id graph collapsed to one record; both duplicates' edges
        // survive and point at the surviving record.
        let source_id = nodes_by_id
            .get(source.id.as_str())
            .map(|node| interner.intern(&node.id))
            .ok_or_else(|| GraphCodecError::Invalid("edge source stable ID is missing".into()))?;
        let target_id = nodes_by_id
            .get(target.id.as_str())
            .map(|node| interner.intern(&node.id))
            .ok_or_else(|| GraphCodecError::Invalid("edge target stable ID is missing".into()))?;
        let edge_type = storage_edge_type(&edge.edge_type);
        let type_code = edge_type_code_v2(edge_type.as_str())?;
        // Last write wins, matching the SQL upsert.
        edges_by_key.insert((source_id, target_id, type_code), {
            let mut metadata = Vec::with_capacity(PDG_EDGE_META_LEN);
            encode_option_u32(&mut metadata, edge.metadata.call_count, "edge call_count")?;
            let variable_name = edge
                .metadata
                .variable_name
                .as_deref()
                .map(|value| interner.intern(value));
            metadata.extend_from_slice(&variable_name.unwrap_or(PDG_V2_NONE).to_le_bytes());
            metadata.extend_from_slice(
                &edge
                    .metadata
                    .confidence
                    .unwrap_or(f32::NAN)
                    .to_bits()
                    .to_le_bytes(),
            );
            let channel = edge
                .metadata
                .channel
                .as_deref()
                .map(|value| interner.intern(value));
            metadata.extend_from_slice(&channel.unwrap_or(PDG_V2_NONE).to_le_bytes());
            encode_option_u32(&mut metadata, edge.metadata.position, "edge position")?;
            metadata
        });
    }
    // Deterministic order: sort by interned id triple, then interleave into
    // the edge record and metadata arrays the layout expects.
    let mut edge_keys: Vec<(u32, u32, u32)> = edges_by_key.keys().copied().collect();
    edge_keys.sort_unstable();
    let mut edges = Vec::with_capacity(edge_keys.len() * PDG_EDGE_LEN);
    let mut edge_metadata = Vec::with_capacity(edge_keys.len() * PDG_EDGE_META_LEN);
    for key @ (source_id, target_id, type_code) in edge_keys {
        edges.extend_from_slice(&source_id.to_le_bytes());
        edges.extend_from_slice(&target_id.to_le_bytes());
        edges.extend_from_slice(&type_code.to_le_bytes());
        edge_metadata.extend_from_slice(&edges_by_key[&key]);
    }

    let (string_table, string_bytes) = interner.into_bytes();
    let num_strings = string_table.len() / PDG_STRING_OFFSET_LEN;
    let mut data = Vec::with_capacity(
        node_records.len()
            + edges.len()
            + edge_metadata.len()
            + string_table.len()
            + string_bytes.len(),
    );
    data.extend_from_slice(&node_records);
    data.extend_from_slice(&edges);
    data.extend_from_slice(&edge_metadata);
    data.extend_from_slice(&string_table);
    data.extend_from_slice(&string_bytes);
    let content_hash = crate::storage::cas::blob::blob_hash(&data);

    let mut payload = Vec::with_capacity(PDG_HEADER_LEN + data.len());
    payload.extend_from_slice(PDG_MAGIC);
    payload.push(2);
    payload.extend_from_slice(&[0, 0, 0]);
    payload.extend_from_slice(&(node_ids.len() as u32).to_le_bytes());
    payload.extend_from_slice(&((edges.len() / PDG_EDGE_LEN) as u32).to_le_bytes());
    payload.extend_from_slice(&(num_strings as u32).to_le_bytes());
    payload.extend_from_slice(&(string_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&content_hash);
    payload.extend_from_slice(&data);

    let index_by_id = node_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index as u32))
        .collect();
    Ok((
        payload,
        LayerNodeAssignment {
            node_ids,
            index_by_id,
        },
    ))
}

/// Encode a `LIDX-SYM1` symbols layer from the in-memory graph.
///
/// Replaces the catalog-based `encode_symbols_layer` on the live publish
/// path (the catalog no longer holds node rows). Field semantics match that
/// encoder: name/type/file from the node, complexity as f32, and line fields
/// zero (the graph's byte ranges live in the Pdg layer; the symbols layer's
/// line columns were never populated by the catalog path either).
pub fn encode_symbols_layer_from_graph(
    pdg: &ProgramDependenceGraph,
) -> Result<Vec<u8>, GraphCodecError> {
    use crate::storage::generation::reader::{SYMBOL_ENTRY_LEN, SYMBOLS_HEADER_LEN, SYMBOLS_MAGIC};

    let mut interner = StringInterner::new();
    let mut symbols: Vec<u8> = Vec::new();
    // Deterministic order, duplicate stable ids collapse last-wins like the
    // Pdg encoder so the two layers describe the same node set.
    let mut nodes_by_id: HashMap<&str, &Node> = HashMap::with_capacity(pdg.node_count());
    for index in pdg.node_indices() {
        let node = pdg
            .get_node(index)
            .ok_or_else(|| GraphCodecError::Invalid("node index has no node".into()))?;
        nodes_by_id.insert(node.id.as_str(), node);
    }
    let mut ids: Vec<&str> = nodes_by_id.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let node = nodes_by_id[id];
        let node_type = storage_node_type(&node.node_type);
        symbols.extend_from_slice(&interner.intern(&node.name).to_le_bytes());
        // v2 storage vocabulary, NOT the lossy legacy_node_type_code map.
        let type_code = node_type_code_v2(node_type.as_str())?;
        symbols.extend_from_slice(&type_code.to_le_bytes());
        symbols.extend_from_slice(&interner.intern(&node.file_path).to_le_bytes());
        symbols.extend_from_slice(&0u32.to_le_bytes()); // start_line (never populated)
        symbols.extend_from_slice(&0u32.to_le_bytes()); // end_line (never populated)
        symbols.extend_from_slice(&(node.complexity as f32).to_le_bytes());
    }

    let (string_table, string_bytes) = interner.into_bytes();
    let num_symbols = symbols.len() / SYMBOL_ENTRY_LEN;
    let num_strings = string_table.len() / PDG_STRING_OFFSET_LEN;
    let mut data = Vec::with_capacity(symbols.len() + string_table.len() + string_bytes.len());
    data.extend_from_slice(&symbols);
    data.extend_from_slice(&string_table);
    data.extend_from_slice(&string_bytes);
    let content_hash = crate::storage::cas::blob::blob_hash(&data);

    let mut payload = Vec::with_capacity(SYMBOLS_HEADER_LEN + data.len());
    payload.extend_from_slice(SYMBOLS_MAGIC);
    payload.push(1);
    payload.extend_from_slice(&[0, 0, 0]);
    payload.extend_from_slice(&(num_symbols as u32).to_le_bytes());
    payload.extend_from_slice(&(num_strings as u32).to_le_bytes());
    payload.extend_from_slice(&(string_bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&content_hash);
    payload.extend_from_slice(&data);
    Ok(payload)
}

fn encode_option_u32(
    output: &mut Vec<u8>,
    value: Option<usize>,
    field: &str,
) -> Result<(), GraphCodecError> {
    let value = value
        .map(u32::try_from)
        .transpose()
        .map_err(|_| GraphCodecError::Invalid(format!("{field} exceeds u32")))?
        .unwrap_or(PDG_V2_NONE);
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

impl PdgReader {
    /// Decode a version-2 PDG layer into a full in-memory graph.
    ///
    /// Rebuilds every `Node` field, edge metadata (sentinels → `None`),
    /// precision markers, and the trigram index. Version-1 payloads are
    /// rejected (they are positional catalog snapshots; hydrate those through
    /// the legacy SQL path).
    pub fn to_program_dependence_graph(&self) -> Result<ProgramDependenceGraph, GraphCodecError> {
        if self.version() != 2 {
            return Err(GraphCodecError::Invalid(format!(
                "cannot hydrate graph from PDG layer version {}",
                self.version()
            )));
        }
        let mut graph = ProgramDependenceGraph::new();
        graph.reserve_nodes(self.num_nodes());
        // node-id string → petgraph NodeId. Duplicate node-id records in the
        // payload collapse last-wins, exactly like the SQL upsert did.
        let mut node_id_to_graph: HashMap<String, crate::graph::pdg::NodeId> = HashMap::new();
        for index in 0..self.num_nodes() {
            let record = self.node_full(index)?;
            let id = required_string(self, record.node_id, "node id")?;
            let name = required_string(self, record.symbol_name, "symbol name")?;
            let file_path = required_string(self, record.file_path, "file path")?;
            let language = required_string(self, record.language, "language")?;
            let node_type = graph_node_type(record.node_type)?;
            let graph_node = graph.add_node_without_trigrams(Node {
                id: id.clone(),
                node_type,
                name,
                file_path: Arc::from(file_path),
                byte_range: (record.byte_start as usize, record.byte_end as usize),
                complexity: record.complexity,
                language,
            });
            if record.precision {
                graph.mark_precision_symbol(id.clone());
            }
            node_id_to_graph.insert(id, graph_node);
        }
        for index in 0..self.num_edges() {
            let edge = self.edge(index)?;
            let source_id = resolve_interned_node(self, edge.src, index, "source")?;
            let target_id = resolve_interned_node(self, edge.dst, index, "target")?;
            let (source, target) = match (
                node_id_to_graph.get(&source_id),
                node_id_to_graph.get(&target_id),
            ) {
                (Some(source), Some(target)) => (*source, *target),
                _ => {
                    return Err(GraphCodecError::Invalid(format!(
                        "edge {index} endpoint node id not present in the node table: {source_id} -> {target_id}"
                    )));
                }
            };
            let edge_type = graph_edge_type(edge.edge_type)?;
            let metadata = self.edge_meta(index)?;
            graph.add_edge(
                source,
                target,
                Edge {
                    edge_type,
                    metadata: EdgeMetadata {
                        call_count: metadata.call_count.map(|value| value as usize),
                        variable_name: metadata
                            .variable_name
                            .map(|id| required_string(self, id, "variable name"))
                            .transpose()?,
                        confidence: metadata.confidence,
                        channel: metadata
                            .channel
                            .map(|id| required_string(self, id, "channel"))
                            .transpose()?,
                        position: metadata.position.map(|value| value as usize),
                    },
                },
            );
        }
        graph.rebuild_trigram_index();
        Ok(graph)
    }
}

/// Resolve an edge-endpoint interned id to its node-id string.
fn resolve_interned_node(
    reader: &PdgReader,
    interned: u32,
    edge_index: usize,
    role: &str,
) -> Result<String, GraphCodecError> {
    reader
        .resolve_string(interned)
        .map(str::to_owned)
        .ok_or_else(|| {
            GraphCodecError::Invalid(format!(
                "edge {edge_index} {role} string id {interned} does not resolve"
            ))
        })
}

fn required_string(reader: &PdgReader, id: u32, field: &str) -> Result<String, GraphCodecError> {
    reader
        .resolve_string(id)
        .map(str::to_owned)
        .ok_or_else(|| GraphCodecError::Invalid(format!("invalid interned {field} string id {id}")))
}

fn graph_node_type(code: u32) -> Result<NodeType, GraphCodecError> {
    let name = node_type_name_v2(code)?;
    StorageNodeType::from_str_name(name)
        .map(storage_node_type_to_graph)
        .ok_or_else(|| {
            GraphCodecError::Invalid(format!(
                "unknown node type {name:?}; refusing to decode a lossless layer"
            ))
        })
}

fn graph_edge_type(code: u32) -> Result<EdgeType, GraphCodecError> {
    let name = edge_type_name_v2(code)?;
    crate::storage::edges::EdgeType::from_str_name(name)
        .map(storage_edge_type_to_graph)
        .ok_or_else(|| {
            GraphCodecError::Invalid(format!(
                "unknown edge type {name:?}; refusing to decode a lossless layer"
            ))
        })
}

/// The graph exactly as a PDG layer round-trip reconstructs it: duplicate
/// stable node ids collapsed last-wins and parallel (source, target, type)
/// edges collapsed last-wins — the semantics both the v2 encoder and the
/// legacy SQL upserts share.
///
/// `persisted_search_identity` fingerprints this form so snapshot/embedder
/// freshness matches what a cold layer hydration rebuilds.
pub fn collapse_for_layer(pdg: &ProgramDependenceGraph) -> ProgramDependenceGraph {
    let mut collapsed = ProgramDependenceGraph::new();
    // node-id string -> collapsed index; a repeat id REPLACES the earlier
    // record's fields in place (last-wins), never adding a second node.
    let mut node_index_by_id: HashMap<String, crate::graph::pdg::NodeId> = HashMap::new();
    for index in pdg.node_indices() {
        let Some(node) = pdg.get_node(index) else {
            continue;
        };
        match node_index_by_id.entry(node.id.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let node_index = *entry.get();
                *collapsed.get_node_mut(node_index).expect("occupied node") = node.clone();
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let new_index = collapsed.add_node_without_trigrams(node.clone());
                entry.insert(new_index);
            }
        }
    }
    // Precision markers are keyed by stable id, so they survive the collapse.
    for id in pdg.precision_symbols() {
        collapsed.mark_precision_symbol(id.clone());
    }
    // Parallel (source, target, type) edges collapse last-wins, matching the
    // encoder's `edges_by_key` and the legacy intel_edges PRIMARY KEY.
    let mut edges_by_key: HashMap<(String, String, EdgeType), Edge> = HashMap::new();
    for edge_id in pdg.edge_indices() {
        let Some(edge) = pdg.get_edge(edge_id) else {
            continue;
        };
        let Some((source, target)) = pdg.edge_endpoints(edge_id) else {
            continue;
        };
        let (Some(source), Some(target)) = (pdg.get_node(source), pdg.get_node(target)) else {
            continue;
        };
        if !node_index_by_id.contains_key(&source.id) || !node_index_by_id.contains_key(&target.id)
        {
            continue;
        }
        let key = (source.id.clone(), target.id.clone(), edge.edge_type.clone());
        // Insert or last-wins update — both drop the earlier record.
        // NaN confidence is the wire absence sentinel, so mirror the decode
        // normalization Some(NaN) -> None.
        let mut edge = edge.clone();
        if edge.metadata.confidence == Some(f32::NAN) {
            edge.metadata.confidence = None;
        }
        edges_by_key.insert(key, edge);
    }
    for ((source_id, target_id, _), edge) in &edges_by_key {
        let source = node_index_by_id[source_id];
        let target = node_index_by_id[target_id];
        collapsed.add_edge(source, target, edge.clone());
    }
    collapsed.rebuild_trigram_index();
    collapsed
}

fn storage_node_type_to_graph(node_type: StorageNodeType) -> NodeType {
    match node_type {
        StorageNodeType::Function => NodeType::Function,
        StorageNodeType::Class => NodeType::Class,
        StorageNodeType::Method => NodeType::Method,
        StorageNodeType::Variable => NodeType::Variable,
        StorageNodeType::Module => NodeType::Module,
        StorageNodeType::External => NodeType::External,
        StorageNodeType::DocSection => NodeType::DocSection,
        StorageNodeType::FileSummary => NodeType::FileSummary,
    }
}

fn storage_edge_type_to_graph(edge_type: crate::storage::edges::EdgeType) -> EdgeType {
    use crate::storage::edges::EdgeType as StorageEdgeType;
    match edge_type {
        StorageEdgeType::Call => EdgeType::Call,
        StorageEdgeType::DataDependency => EdgeType::DataDependency,
        StorageEdgeType::Inheritance => EdgeType::Inheritance,
        StorageEdgeType::Import => EdgeType::Import,
        StorageEdgeType::Containment => EdgeType::Containment,
        StorageEdgeType::TypeOf => EdgeType::TypeOf,
        StorageEdgeType::StateTransition => EdgeType::StateTransition,
        StorageEdgeType::CommandArgument => EdgeType::CommandArgument,
        StorageEdgeType::Environment => EdgeType::Environment,
        StorageEdgeType::Stdin => EdgeType::Stdin,
    }
}

fn storage_node_type(node_type: &NodeType) -> StorageNodeType {
    match node_type {
        NodeType::Function => StorageNodeType::Function,
        NodeType::Class => StorageNodeType::Class,
        NodeType::Method => StorageNodeType::Method,
        NodeType::Variable => StorageNodeType::Variable,
        NodeType::Module => StorageNodeType::Module,
        NodeType::External => StorageNodeType::External,
        NodeType::DocSection => StorageNodeType::DocSection,
        NodeType::FileSummary => StorageNodeType::FileSummary,
    }
}

fn storage_edge_type(edge_type: &EdgeType) -> crate::storage::edges::EdgeType {
    use crate::storage::edges::EdgeType as StorageEdgeType;
    match edge_type {
        EdgeType::Call => StorageEdgeType::Call,
        EdgeType::DataDependency => StorageEdgeType::DataDependency,
        EdgeType::Inheritance => StorageEdgeType::Inheritance,
        EdgeType::Import => StorageEdgeType::Import,
        EdgeType::Containment => StorageEdgeType::Containment,
        EdgeType::TypeOf => StorageEdgeType::TypeOf,
        EdgeType::StateTransition => StorageEdgeType::StateTransition,
        EdgeType::CommandArgument => StorageEdgeType::CommandArgument,
        EdgeType::Environment => StorageEdgeType::Environment,
        EdgeType::Stdin => StorageEdgeType::Stdin,
    }
}

#[cfg(test)]
#[path = "graph_codec_test.rs"]
mod tests;
