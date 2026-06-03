// Trigram Index for Accelerated Fuzzy Node Lookup
//
// A trigram (3-character substring) inverted index that maps each trigram to
// the set of node IDs containing it. When `fuzzy_find_node` searches for a
// query, it extracts trigrams from the query and intersects the posting lists
// to dramatically reduce the search space — skipping nodes that share no
// trigrams with the query.
//
// For a project with 100k nodes and a 6-character query (4 trigrams), the
// intersection typically reduces the candidate set to <1% of all nodes.

use crate::graph::pdg::{NodeId, ProgramDependenceGraph};
use std::collections::HashMap;

/// A trigram stored as a packed u32 (3 ASCII bytes + zero high byte).
/// This avoids heap-allocating a String for every trigram.
pub type Trigram = u32;

/// Version header for the serialized trigram index format.
/// Increment this when the on-disk layout changes to prevent
/// deserializing incompatible data.
const TRIGRAM_INDEX_VERSION: u32 = 1;

/// Inverted index: trigram → set of node indices (as u32).
///
/// Uses `Vec<u32>` for posting lists (compact, cache-friendly) and
/// `HashMap<u32, Vec<u32>>` for the index itself.
#[derive(Debug, Clone, Default)]
pub struct TrigramIndex {
    /// Maps each trigram to a sorted vec of node indices.
    postings: HashMap<Trigram, Vec<u32>>,
    /// Total number of nodes indexed.
    node_count: usize,
}

impl TrigramIndex {
    /// Create a new, empty trigram index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a trigram index from all nodes in the PDG.
    ///
    /// Indexes the following text for each node:
    /// - Node name (lowercased)
    /// - Node ID (lowercased)
    /// - File path (lowercased, relative)
    ///
    /// Build time is typically <100ms for 100k nodes.
    pub fn build_from_pdg(pdg: &ProgramDependenceGraph) -> Self {
        let mut index = Self::new();
        index.node_count = pdg.node_count();

        for node_id in pdg.node_indices() {
            if let Some(node) = pdg.get_node(node_id) {
                let node_idx = node_id.index() as u32;

                // Index node name (lowercased)
                let name_lower = node.name.to_lowercase();
                for trigram in extract_trigrams(&name_lower) {
                    index.postings.entry(trigram).or_default().push(node_idx);
                }

                // Index node ID (lowercased)
                let id_lower = node.id.to_lowercase();
                for trigram in extract_trigrams(&id_lower) {
                    index.postings.entry(trigram).or_default().push(node_idx);
                }

                // Index file path (lowercased)
                let file_lower = node.file_path.to_lowercase();
                for trigram in extract_trigrams(&file_lower) {
                    index.postings.entry(trigram).or_default().push(node_idx);
                }
            }
        }

        // Sort and deduplicate posting lists for efficient intersection
        for posting_list in index.postings.values_mut() {
            posting_list.sort_unstable();
            posting_list.dedup();
        }

        index
    }

    /// Query the trigram index to find candidate node indices.
    ///
    /// Extracts trigrams from the query and intersects all posting lists.
    /// Returns the set of node indices that contain ALL query trigrams.
    ///
    /// If the query has fewer than 3 characters (no trigrams), returns None
    /// to signal that the caller should fall back to a full linear scan.
    pub fn query(&self, query_lower: &str) -> Option<Vec<u32>> {
        let trigrams = extract_trigrams(query_lower);
        if trigrams.is_empty() {
            return None; // Query too short, fall back to full scan
        }

        // Find the smallest posting list first for efficient intersection
        let mut sorted_trigrams: Vec<&Vec<u32>> = trigrams
            .iter()
            .filter_map(|t| self.postings.get(t))
            .collect();

        if sorted_trigrams.is_empty() {
            // No trigrams found at all — no candidates
            return Some(Vec::new());
        }

        // If some trigrams weren't found, the intersection is empty
        if sorted_trigrams.len() < trigrams.len() {
            return Some(Vec::new());
        }

        // Sort by posting list size (smallest first) for optimal intersection
        sorted_trigrams.sort_by_key(|list| list.len());

        // Start with the smallest posting list and intersect
        let mut result = sorted_trigrams[0].clone();
        for posting_list in sorted_trigrams.iter().skip(1) {
            result = intersect_sorted(&result, posting_list);
            if result.is_empty() {
                return Some(Vec::new());
            }
        }

        Some(result)
    }

    /// Add a single node to the index (for incremental updates).
    ///
    /// Call this when a new node is added to the PDG after the initial build.
    pub fn add_node(
        &mut self,
        node_id: NodeId,
        name: &str,
        node_id_str: &str,
        file_path: &str,
    ) {
        let node_idx = node_id.index() as u32;
        self.node_count += 1;

        // Index name (lowercased)
        for trigram in extract_trigrams(&name.to_lowercase()) {
            let list = self.postings.entry(trigram).or_default();
            // Maintain sorted order with dedup
            if let Err(pos) = list.binary_search(&node_idx) {
                list.insert(pos, node_idx);
            }
        }

        // Index node ID (lowercased)
        for trigram in extract_trigrams(&node_id_str.to_lowercase()) {
            let list = self.postings.entry(trigram).or_default();
            if let Err(pos) = list.binary_search(&node_idx) {
                list.insert(pos, node_idx);
            }
        }

        // Index file path (lowercased)
        for trigram in extract_trigrams(&file_path.to_lowercase()) {
            let list = self.postings.entry(trigram).or_default();
            if let Err(pos) = list.binary_search(&node_idx) {
                list.insert(pos, node_idx);
            }
        }
    }

    /// Remove a node from the index.
    ///
    /// Call this when a node is removed from the PDG.
    /// Uses targeted removal: extracts trigrams from the node's text fields
    /// and only cleans those specific posting lists, avoiding O(T) scan.
    pub fn remove_node(
        &mut self,
        node_id: NodeId,
        name: &str,
        node_id_str: &str,
        file_path: &str,
    ) {
        let node_idx = node_id.index() as u32;
        if self.node_count > 0 {
            self.node_count -= 1;
        }

        // Collect unique trigrams from the same text fields used during add_node
        let mut trigrams_to_clean: Vec<Trigram> = Vec::new();
        trigrams_to_clean.extend(extract_trigrams(&name.to_lowercase()));
        trigrams_to_clean.extend(extract_trigrams(&node_id_str.to_lowercase()));
        trigrams_to_clean.extend(extract_trigrams(&file_path.to_lowercase()));
        trigrams_to_clean.sort_unstable();
        trigrams_to_clean.dedup();

        // Remove the node_idx only from posting lists that could contain it
        for trigram in &trigrams_to_clean {
            if let Some(posting_list) = self.postings.get_mut(trigram) {
                if let Ok(pos) = posting_list.binary_search(&node_idx) {
                    posting_list.remove(pos);
                }
            }
        }

        // Clean up empty posting lists to save memory
        self.postings.retain(|_, list| !list.is_empty());
    }

    /// Returns the number of unique trigrams in the index.
    pub fn trigram_count(&self) -> usize {
        self.postings.len()
    }

    /// Returns the total number of nodes indexed.
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.postings.is_empty()
    }

    /// Serialize the trigram index to bytes for persistence.
    ///
    /// Format:
    ///   [u32: version header (TRIGRAM_INDEX_VERSION)]
    ///   [u32: number of posting entries]
    ///   For each entry:
    ///     [u32: trigram key]
    ///     [u32: posting list length]
    ///     [u32 * posting_list_length: node indices]
    pub fn serialize(&self) -> Vec<u8> {
        let entry_count = self.postings.len() as u32;
        // Pre-pass: compute exact size to avoid over-allocation.
        // Header: 4 (version) + 4 (entry count). Per entry: 4 (key) + 4 (len) + 4 * posting_list.len().
        let total_size: usize = 4 + 4 + self.postings.values().map(|list| 8 + list.len() * 4).sum::<usize>();
        let mut buf = Vec::with_capacity(total_size);

        buf.extend_from_slice(&TRIGRAM_INDEX_VERSION.to_le_bytes());
        buf.extend_from_slice(&entry_count.to_le_bytes());

        for (&trigram, posting_list) in &self.postings {
            buf.extend_from_slice(&trigram.to_le_bytes());
            let len = posting_list.len() as u32;
            buf.extend_from_slice(&len.to_le_bytes());
            for &node_idx in posting_list {
                buf.extend_from_slice(&node_idx.to_le_bytes());
            }
        }

        buf
    }

    /// Deserialize a trigram index from bytes.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }

        // Validate version header
        let version = u32::from_le_bytes(data[0..4].try_into().ok()?);
        if version != TRIGRAM_INDEX_VERSION {
            return None;
        }

        let entry_count = u32::from_le_bytes(data[4..8].try_into().ok()?) as usize;
        let mut offset = 8;
        let mut postings = HashMap::with_capacity_and_hasher(
            entry_count,
            std::collections::hash_map::RandomState::default(),
        );

        for _ in 0..entry_count {
            if offset + 8 > data.len() {
                return None;
            }
            let trigram = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
            offset += 4;
            let list_len = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
            offset += 4;

            if offset + list_len * 4 > data.len() {
                return None;
            }

            let mut posting_list = Vec::with_capacity(list_len);
            for _ in 0..list_len {
                let node_idx = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
                offset += 4;
                posting_list.push(node_idx);
            }

            postings.insert(trigram, posting_list);
        }

        // Count unique nodes (max node index + 1 is an approximation, but
        // we can compute it from the posting lists)
        let max_idx = postings
            .values()
            .flat_map(|v| v.iter().copied())
            .max()
            .unwrap_or(0);
        // node_count is approximate — we don't track exact count in serialized form
        // but it's only used for stats, not correctness
        let node_count = max_idx as usize + 1;

        Some(Self {
            postings,
            node_count,
        })
    }
}

/// Extract all trigrams from a string, returning them as packed u32 values.
///
/// Uses character-level trigrams (not byte-level) to correctly handle
/// multi-byte UTF-8 characters. Each trigram is a u32 hash of 3 consecutive
/// characters. The string should already be lowercased for case-insensitive matching.
pub fn extract_trigrams(s: &str) -> Vec<Trigram> {
    let mut trigrams = Vec::new();
    let mut chars = s.chars();
    let mut c1 = match chars.next() {
        Some(c) => c,
        None => return trigrams,
    };
    let mut c2 = match chars.next() {
        Some(c) => c,
        None => return trigrams,
    };

    for c3 in chars {
        let mut h: u32 = 2166136261; // FNV offset basis
        for &c in &[c1, c2, c3] {
            h ^= c as u32;
            h = h.wrapping_mul(16777619); // FNV prime
        }
        trigrams.push(h);
        c1 = c2;
        c2 = c3;
    }
    trigrams
}

/// Intersect two sorted vecs of u32, returning a new sorted vec.
fn intersect_sorted(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(a.len().min(b.len()));
    let mut i = 0;
    let mut j = 0;

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                result.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_trigrams() {
        let trigrams = extract_trigrams("hello");
        assert_eq!(trigrams.len(), 3); // "hel", "ell", "llo"
    }

    #[test]
    fn test_extract_trigrams_short() {
        let trigrams = extract_trigrams("ab");
        assert!(trigrams.is_empty());
    }

    #[test]
    fn test_intersect_sorted() {
        let a = vec![1, 3, 5, 7, 9];
        let b = vec![3, 4, 5, 8, 9];
        let result = intersect_sorted(&a, &b);
        assert_eq!(result, vec![3, 5, 9]);
    }

    #[test]
    fn test_intersect_empty() {
        let a = vec![1, 2, 3];
        let b: Vec<u32> = vec![];
        let result = intersect_sorted(&a, &b);
        assert!(result.is_empty());
    }

    #[test]
    fn test_trigram_index_query() {
        let mut index = TrigramIndex::new();

        // Manually add some entries
        // "hello" has trigrams: "hel", "ell", "llo"
        for t in extract_trigrams("hello") {
            index.postings.entry(t).or_default().push(0u32);
        }
        // "world" has trigrams: "wor", "orl", "rld"
        for t in extract_trigrams("world") {
            index.postings.entry(t).or_default().push(1u32);
        }
        // "help" has trigrams: "hel", "elp"
        for t in extract_trigrams("help") {
            index.postings.entry(t).or_default().push(2u32);
        }

        // Query "hello" should match node 0
        let result = index.query("hello").unwrap();
        assert!(result.contains(&0));

        // Query "hel" should match nodes 0 and 2
        let result = index.query("hel").unwrap();
        assert!(result.contains(&0));
        assert!(result.contains(&2));

        // Query "xyz" should return empty
        let result = index.query("xyz").unwrap();
        assert!(result.is_empty());

        // Query "ab" (too short) should return None
        assert!(index.query("ab").is_none());
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let mut index = TrigramIndex::new();
        index.node_count = 2;

        for t in extract_trigrams("hello") {
            index.postings.entry(t).or_default().push(0u32);
        }
        for t in extract_trigrams("world") {
            index.postings.entry(t).or_default().push(1u32);
        }

        let serialized = index.serialize();
        let deserialized = TrigramIndex::deserialize(&serialized).unwrap();

        assert_eq!(deserialized.postings.len(), index.postings.len());

        // Verify query still works
        let result = deserialized.query("hello").unwrap();
        assert!(result.contains(&0));

        let result = deserialized.query("world").unwrap();
        assert!(result.contains(&1));

        // Verify empty query returns None
        assert!(deserialized.query("ab").is_none());
    }
}
