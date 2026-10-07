use super::*;

impl ProgramDependenceGraph {
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
        // Neighbour buffer reused across levels. It was a `Mutex` field shared
        // by every traversal, which serialized the parallel indexing passes
        // (two traversals per symbol across all cores) on one lock.
        let mut scratch: Vec<NodeId> = Vec::new();

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

    pub(super) fn bfs_directed(
        &self,
        start: NodeId,
        config: &TraversalConfig,
        dir: Direction,
    ) -> Vec<NodeId> {
        let mut visited: HashSet<NodeId> = HashSet::default();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
        let mut result: Vec<NodeId> = Vec::new();
        let mut scratch: Vec<NodeId> = Vec::new();

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
        bincode::serialize(&SerializablePDGRef::from_pdg(self))
            .map_err(|e| format!("Serialize failed: {}", e))
    }

    /// Deserializes a PDG from binary data.
    ///
    /// Restores a ProgramDependenceGraph from bytes previously serialized with `serialize()`.
    /// Payloads written before precision markers were introduced are accepted
    /// with an empty marker set.
    ///
    /// # Arguments
    ///
    /// * `data` - The binary data to deserialize
    ///
    /// # Returns
    ///
    /// A Result containing the deserialized PDG, or an error message if deserialization fails.
    pub fn deserialize(data: &[u8]) -> Result<Self, String> {
        let mut errors = Vec::new();
        deserialize_schema::<SerializablePDG>(data, &mut errors)
            .or_else(|| deserialize_schema::<SerializablePDGWithoutPrecision>(data, &mut errors))
            .or_else(|| deserialize_schema::<SerializablePDGWithoutEmbeddings>(data, &mut errors))
            .or_else(|| {
                deserialize_schema::<SerializablePDGWithoutEmbeddingsAndNameLower>(
                    data,
                    &mut errors,
                )
            })
            .or_else(|| {
                deserialize_schema::<SerializablePDGWithInlineEmbeddings>(data, &mut errors)
            })
            .or_else(|| {
                deserialize_schema::<SerializablePDGWithoutInlineEmbeddings>(data, &mut errors)
            })
            .or_else(|| {
                deserialize_schema::<SerializablePDGWithInlineEmbeddingsWithoutNameLower>(
                    data,
                    &mut errors,
                )
            })
            .or_else(|| {
                deserialize_schema::<SerializablePDGWithoutInlineEmbeddingsAndNameLower>(
                    data,
                    &mut errors,
                )
            })
            .ok_or_else(|| format!("Deserialize failed: {}", errors.join("; ")))
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
