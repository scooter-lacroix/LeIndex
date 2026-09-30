//! Integer-addressed inverted index for the resident text search.
//!
//! A mid-size project has roughly a million (token, node) pairs. Holding them
//! as two hash-of-hash-of-`Arc<str>` structures (token → nodes and node →
//! tokens) cost about 600 ms to rebuild on every cold start and ~100 MB of
//! resident memory, almost all of it hashing and pointer overhead.
//!
//! Here each distinct token and each node gets a dense `u32`; postings and
//! per-node token lists are sorted `Vec<u32>`. Building from a persisted
//! dictionary is pushes into vectors — no string hashing per pair — and lookups
//! are a single dictionary probe plus a slice scan.
//!
//! Node handles are never reused, so postings stay ascending by construction
//! (nodes are only ever appended) and removal is a binary search.

use std::sync::Arc;

use super::fx::FastMap;

/// Token dictionary, postings and per-node token lists.
#[derive(Default, Clone)]
pub(super) struct TokenIndex {
    /// Live token → id. A token whose last posting was removed leaves this map.
    ids: FastMap<Arc<str>, u32>,
    /// id → token. Ids of removed tokens stay behind as tombstones.
    tokens: Vec<Arc<str>>,
    /// id → ascending node handles that contain the token.
    postings: Vec<Vec<u32>>,
    /// handle → node id (empty for a removed node).
    keys: Vec<Arc<str>>,
    /// Live node id → handle.
    handles: FastMap<Arc<str>, u32>,
    /// handle → ascending token ids of the node.
    node_tokens: Vec<Vec<u32>>,
}

impl TokenIndex {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    fn intern(&mut self, token: &str) -> u32 {
        if let Some(&id) = self.ids.get(token) {
            return id;
        }
        let id = self.tokens.len() as u32;
        let shared: Arc<str> = Arc::from(token);
        self.tokens.push(Arc::clone(&shared));
        self.postings.push(Vec::new());
        self.ids.insert(shared, id);
        id
    }

    /// Add a node with the given (possibly repeated) tokens. The node must not
    /// be live already; callers remove the previous version first.
    pub(super) fn insert_node<'a>(
        &mut self,
        node_id: &str,
        tokens: impl IntoIterator<Item = &'a str>,
    ) {
        let mut ids: Vec<u32> = tokens.into_iter().map(|t| self.intern(t)).collect();
        ids.sort_unstable();
        ids.dedup();
        let handle = self.keys.len() as u32;
        for &id in &ids {
            self.postings[id as usize].push(handle);
        }
        let key: Arc<str> = Arc::from(node_id);
        self.keys.push(Arc::clone(&key));
        self.handles.insert(key, handle);
        self.node_tokens.push(ids);
    }

    /// Remove a node from every posting. No-op when it is not live.
    pub(super) fn remove_node(&mut self, node_id: &str) {
        let Some(handle) = self.handles.remove(node_id) else {
            return;
        };
        let ids = std::mem::take(&mut self.node_tokens[handle as usize]);
        for id in ids {
            let posting = &mut self.postings[id as usize];
            if let Ok(pos) = posting.binary_search(&handle) {
                posting.remove(pos);
            }
            if posting.is_empty() {
                *posting = Vec::new();
                let token = Arc::clone(&self.tokens[id as usize]);
                self.ids.remove(&*token);
            }
        }
        self.keys[handle as usize] = Arc::from("");
    }

    /// Node ids that contain `token`.
    pub(super) fn nodes_with_token<'a>(
        &'a self,
        token: &str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let posting: &[u32] = self
            .ids
            .get(token)
            .map_or(&[][..], |&id| &self.postings[id as usize]);
        posting.iter().map(|&h| &*self.keys[h as usize])
    }

    /// Whether the node contains `token`.
    pub(super) fn node_has_token(&self, node_id: &str, token: &str) -> bool {
        let (Some(&handle), Some(&id)) = (self.handles.get(node_id), self.ids.get(token)) else {
            return false;
        };
        self.node_tokens[handle as usize].binary_search(&id).is_ok()
    }

    pub(super) fn has_node(&self, node_id: &str) -> bool {
        self.handles.contains_key(node_id)
    }

    #[cfg(test)]
    pub(super) fn has_token(&self, token: &str) -> bool {
        self.ids.contains_key(token)
    }

    /// Tokens of one node, ascending by id.
    pub(super) fn tokens_of_node(&self, node_id: &str) -> Option<Vec<&str>> {
        let &handle = self.handles.get(node_id)?;
        Some(
            self.node_tokens[handle as usize]
                .iter()
                .map(|&id| &*self.tokens[id as usize])
                .collect(),
        )
    }

    pub(super) fn node_count(&self) -> usize {
        self.handles.len()
    }

    pub(super) fn token_count(&self) -> usize {
        self.ids.len()
    }

    /// Every live `(token, node ids)` pair, for validation and compaction.
    pub(super) fn entries(&self) -> impl Iterator<Item = (&str, impl Iterator<Item = &str>)> {
        self.ids.iter().map(move |(token, &id)| {
            (
                &**token,
                self.postings[id as usize]
                    .iter()
                    .map(move |&h| &*self.keys[h as usize]),
            )
        })
    }

    pub(super) fn estimated_bytes(&self) -> usize {
        let pair = std::mem::size_of::<u32>();
        let dictionary: usize = self.tokens.iter().map(|t| t.len() + 16).sum();
        let keys: usize = self.keys.iter().map(|k| k.len() + 16).sum();
        let postings: usize = self.postings.iter().map(|p| p.len() * pair + 24).sum();
        let per_node: usize = self.node_tokens.iter().map(|n| n.len() * pair + 24).sum();
        dictionary + keys + postings + per_node
    }

    /// Persistable form for the nodes in `order`: a compact dictionary of the
    /// tokens still in use, and for each node its ascending ids into it.
    pub(super) fn to_dictionary<'a>(
        &self,
        order: impl Iterator<Item = &'a str>,
    ) -> (Vec<String>, Vec<Vec<u32>>) {
        let mut remap: Vec<u32> = vec![u32::MAX; self.tokens.len()];
        let mut dictionary: Vec<String> = Vec::with_capacity(self.ids.len());
        // Ascending by old id keeps the dictionary order deterministic.
        let mut live: Vec<u32> = self.ids.values().copied().collect();
        live.sort_unstable();
        for id in live {
            remap[id as usize] = dictionary.len() as u32;
            dictionary.push(self.tokens[id as usize].to_string());
        }
        let per_node = order
            .map(|node_id| {
                let mut ids: Vec<u32> = self
                    .handles
                    .get(node_id)
                    .map(|&h| {
                        self.node_tokens[h as usize]
                            .iter()
                            .map(|&id| remap[id as usize])
                            .collect()
                    })
                    .unwrap_or_default();
                ids.sort_unstable();
                ids
            })
            .collect();
        (dictionary, per_node)
    }

    /// Rebuild from a persisted dictionary. `node_ids[i]` owns `per_node[i]`.
    /// Returns `None` if any id is out of range or a list is not ascending.
    pub(super) fn from_dictionary<'a>(
        dictionary: &[String],
        node_ids: impl ExactSizeIterator<Item = &'a str>,
        per_node: Vec<Vec<u32>>,
    ) -> Option<Self> {
        if node_ids.len() != per_node.len() {
            return None;
        }
        let tokens: Vec<Arc<str>> = dictionary.iter().map(|t| Arc::from(t.as_str())).collect();
        let mut ids: FastMap<Arc<str>, u32> =
            FastMap::with_capacity_and_hasher(tokens.len(), Default::default());
        for (id, token) in tokens.iter().enumerate() {
            ids.insert(Arc::clone(token), id as u32);
        }
        if ids.len() != tokens.len() {
            return None;
        }
        let mut sizes = vec![0u32; tokens.len()];
        for list in &per_node {
            let mut previous: Option<u32> = None;
            for &id in list {
                if id as usize >= tokens.len() || previous.is_some_and(|p| p >= id) {
                    return None;
                }
                previous = Some(id);
                sizes[id as usize] += 1;
            }
        }
        let mut postings: Vec<Vec<u32>> = sizes
            .iter()
            .map(|&n| Vec::with_capacity(n as usize))
            .collect();
        let mut keys: Vec<Arc<str>> = Vec::with_capacity(per_node.len());
        let mut handles: FastMap<Arc<str>, u32> =
            FastMap::with_capacity_and_hasher(per_node.len(), Default::default());
        for (handle, (node_id, list)) in node_ids.zip(&per_node).enumerate() {
            for &id in list {
                postings[id as usize].push(handle as u32);
            }
            let key: Arc<str> = Arc::from(node_id);
            keys.push(Arc::clone(&key));
            handles.insert(key, handle as u32);
        }
        // Tokens nobody uses are not live.
        ids.retain(|_, id| sizes[*id as usize] > 0);
        Some(Self {
            ids,
            tokens,
            postings,
            keys,
            handles,
            node_tokens: per_node,
        })
    }
}

/// A small read-only set of names returned by the public lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrSet<'a>(Vec<&'a str>);

impl<'a> StrSet<'a> {
    pub(super) fn new(mut items: Vec<&'a str>) -> Self {
        items.sort_unstable();
        items.dedup();
        Self(items)
    }

    /// Whether `item` is in the set.
    pub fn contains(&self, item: &str) -> bool {
        self.0.binary_search(&item).is_ok()
    }

    /// Number of names.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the set has no names.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Names in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.0.iter().copied()
    }
}

impl<'a> IntoIterator for StrSet<'a> {
    type Item = &'a str;
    type IntoIter = std::vec::IntoIter<&'a str>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl PartialEq for TokenIndex {
    /// Equal when they index the same node → token relation, whatever the
    /// internal ids and handles are.
    fn eq(&self, other: &Self) -> bool {
        self.canonical() == other.canonical()
    }
}

impl std::fmt::Debug for TokenIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenIndex")
            .field("tokens", &self.token_count())
            .field("nodes", &self.node_count())
            .finish()
    }
}

impl TokenIndex {
    fn canonical(&self) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
        self.entries()
            .map(|(token, nodes)| (token.to_string(), nodes.map(str::to_string).collect()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build() -> TokenIndex {
        let mut index = TokenIndex::default();
        index.insert_node("a", ["alpha", "beta", "alpha"]);
        index.insert_node("b", ["beta", "gamma"]);
        index.insert_node("c", ["gamma"]);
        index
    }

    #[test]
    fn test_token_index_postings_and_membership() {
        let index = build();
        let mut beta: Vec<&str> = index.nodes_with_token("beta").collect();
        beta.sort_unstable();
        assert_eq!(beta, ["a", "b"]);
        assert!(index.node_has_token("a", "alpha"));
        assert!(!index.node_has_token("b", "alpha"));
        assert!(!index.node_has_token("zzz", "alpha"));
        assert_eq!(index.tokens_of_node("a").unwrap().len(), 2, "deduplicated");
        assert_eq!(index.token_count(), 3);
        assert_eq!(index.node_count(), 3);
    }

    #[test]
    fn test_token_index_remove_drops_dead_tokens_and_keeps_others() {
        let mut index = build();
        index.remove_node("a");
        assert!(!index.has_token("alpha"), "last posting removed");
        assert!(index.has_token("beta"));
        assert!(!index.has_node("a"));
        assert_eq!(index.nodes_with_token("beta").collect::<Vec<_>>(), ["b"]);
        // Re-adding after removal works and uses a fresh handle.
        index.insert_node("a", ["alpha"]);
        assert!(index.node_has_token("a", "alpha"));
        assert_eq!(index.nodes_with_token("alpha").collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn test_token_index_dictionary_round_trip_is_equal() {
        let mut index = build();
        index.remove_node("b");
        index.insert_node("d", ["delta", "gamma"]);
        let order = ["a", "c", "d"];
        let (dictionary, per_node) = index.to_dictionary(order.iter().copied());
        assert!(!dictionary.iter().any(|t| t == "beta") || index.has_token("beta"));
        let restored =
            TokenIndex::from_dictionary(&dictionary, order.iter().copied(), per_node).unwrap();
        assert_eq!(restored, index);
        assert!(restored.node_has_token("d", "delta"));
    }

    #[test]
    fn test_token_index_rejects_corrupt_dictionary() {
        let dictionary = vec!["a".to_string(), "b".to_string()];
        assert!(
            TokenIndex::from_dictionary(&dictionary, ["n"].into_iter(), vec![vec![5]]).is_none()
        );
        assert!(
            TokenIndex::from_dictionary(&dictionary, ["n"].into_iter(), vec![vec![1, 0]]).is_none(),
            "ids must ascend"
        );
        assert!(
            TokenIndex::from_dictionary(&dictionary, ["n", "m"].into_iter(), vec![vec![]])
                .is_none()
        );
    }
}
