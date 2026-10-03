use super::*;

impl ProgramDependenceGraph {
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

    pub(super) fn select_name_candidate(
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

    pub(super) fn find_substring_name_match(
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

    pub(super) fn node_contains_name(&self, node_id: NodeId, name_lower: &str) -> bool {
        self.graph.node_weight(node_id).is_some_and(|node| {
            node.name.to_lowercase().contains(name_lower)
                || node.id.to_lowercase().contains(name_lower)
        })
    }
}
