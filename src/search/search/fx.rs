//! A small, fast, non-cryptographic hasher for the resident text index.
//!
//! The inverted index holds roughly a million (token, node) pairs on a mid-size
//! project. Hashing them with the default SipHash was a measurable share of
//! cold start; these maps are process-local and never keyed by untrusted
//! input, so HashDoS resistance buys nothing here. (Same multiply-rotate mix
//! as rustc's `FxHasher`.)

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// Multiply-rotate hasher.
#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word) ^ ((rest.len() as u64) << 56));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Fold the high bits down: hashbrown uses both ends of the hash.
        self.hash ^ (self.hash >> 32)
    }
}

/// `HashMap` with [`FxHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
/// `HashSet` with [`FxHasher`].
pub type FastSet<T> = HashSet<T, BuildHasherDefault<FxHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_fast_containers_behave_like_std_ones() {
        let mut map: FastMap<Arc<str>, u32> = FastMap::default();
        for (i, word) in ["alpha", "beta", "a-much-longer-identifier-name", ""]
            .iter()
            .enumerate()
        {
            map.insert(Arc::from(*word), i as u32);
        }
        assert_eq!(map.get("beta"), Some(&1));
        assert_eq!(map.get("a-much-longer-identifier-name"), Some(&2));
        assert_eq!(map.get(""), Some(&3));
        assert_eq!(map.get("gamma"), None);
        let set: FastSet<Arc<str>> = map.keys().cloned().collect();
        assert!(set.contains("alpha") && !set.contains("zeta"));
    }

    #[test]
    fn test_hash_distinguishes_prefixes_and_lengths() {
        use std::hash::{BuildHasher, BuildHasherDefault};
        let build = BuildHasherDefault::<FxHasher>::default();
        let hash = |s: &str| build.hash_one(s);
        assert_ne!(hash("ab"), hash("abc"));
        assert_ne!(hash("abcdefgh"), hash("abcdefghi"));
        assert_ne!(hash("abcdefgh\0"), hash("abcdefgh"));
    }
}
