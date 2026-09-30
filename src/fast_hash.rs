//! Fast, deterministic non-cryptographic hashing for internal maps and sets.
//!
//! The standard library's default SipHash is DoS-resistant, which buys nothing
//! for maps keyed by node ids, symbol names or trigrams derived from the user's
//! own source tree, while costing a large share of index build and graph
//! hydration time. [`FastHasher`] consumes eight bytes per step with a 128-bit
//! folded multiply (the "mum" primitive used by wyhash/foldhash), so strings
//! are hashed a word at a time and both the high bits (used by hashbrown's
//! control bytes) and low bits (used for bucket selection) are well mixed.
//!
//! The hasher is seedless, so iteration order of a given map is a pure
//! function of its insertion history. It is not suitable for keys chosen by an
//! untrusted party on a network boundary.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const K0: u64 = 0x9E37_79B9_7F4A_7C15;
const K1: u64 = 0xD6E8_FEB8_6659_FD93;
const K2: u64 = 0xA076_1D64_78BD_642F;

/// `HashMap` using [`FastHasher`]. Construct with `FastMap::default()` or
/// `FastMap::with_capacity_and_hasher(n, Default::default())`.
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// `HashSet` using [`FastHasher`].
pub type FastSet<T> = HashSet<T, BuildHasherDefault<FastHasher>>;

/// Create a [`FastMap`] with room for `capacity` entries.
#[inline]
pub fn fast_map_with_capacity<K, V>(capacity: usize) -> FastMap<K, V> {
    FastMap::with_capacity_and_hasher(capacity, BuildHasherDefault::default())
}

/// Create a [`FastSet`] with room for `capacity` entries.
#[inline]
pub fn fast_set_with_capacity<T>(capacity: usize) -> FastSet<T> {
    FastSet::with_capacity_and_hasher(capacity, BuildHasherDefault::default())
}

#[inline(always)]
fn fold_mul(a: u64, b: u64) -> u64 {
    let product = u128::from(a) * u128::from(b);
    (product as u64) ^ ((product >> 64) as u64)
}

/// Multiplicative-fold hasher. See the module docs for the design and limits.
#[derive(Default, Clone, Copy)]
pub struct FastHasher {
    state: u64,
}

impl FastHasher {
    #[inline(always)]
    fn absorb(&mut self, word: u64) {
        self.state = fold_mul(self.state ^ word, K0);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().expect("chunk of 8"));
            self.state = fold_mul(self.state ^ word, K1);
        }
        let tail = chunks.remainder();
        if !tail.is_empty() {
            // Pack up to 7 tail bytes plus the tail length so that "a" and
            // "a\0" (and prefixes of longer inputs) cannot collide by padding.
            let mut buf = [0u8; 8];
            buf[..tail.len()].copy_from_slice(tail);
            let word = u64::from_le_bytes(buf) ^ ((tail.len() as u64) << 56);
            self.state = fold_mul(self.state ^ word, K2);
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.absorb(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.absorb(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.absorb(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.absorb(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.absorb(value as u64);
    }

    #[inline]
    fn write_i32(&mut self, value: i32) {
        self.absorb(value as u32 as u64);
    }

    #[inline]
    fn write_i64(&mut self, value: i64) {
        self.absorb(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Final avalanche so the low bits (bucket index) depend on every input
        // bit, even for dense sequential integer keys.
        fold_mul(self.state ^ K2, K1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{BuildHasher, Hash};

    fn hash_of<T: Hash + ?Sized>(value: &T) -> u64 {
        BuildHasherDefault::<FastHasher>::default().hash_one(value)
    }

    #[test]
    fn test_hash_is_deterministic_across_instances() {
        assert_eq!(hash_of("symbol_name"), hash_of("symbol_name"));
        assert_eq!(hash_of(&42u64), hash_of(&42u64));
        assert_ne!(hash_of("symbol_name"), hash_of("symbol_namf"));
    }

    #[test]
    fn test_string_tails_and_padding_do_not_collide() {
        // Same bytes modulo zero padding, and prefixes of longer strings.
        assert_ne!(hash_of("a"), hash_of("a\0"));
        assert_ne!(hash_of(&b"abcdefgh"[..]), hash_of(&b"abcdefgh\0"[..]));
        let mut seen = std::collections::HashSet::new();
        let base = "abcdefghijklmnopqrstuvwxyz";
        for end in 0..=base.len() {
            assert!(seen.insert(hash_of(&base[..end])), "prefix {end} collided");
        }
    }

    /// Fraction of output bits that flip when a single input bit flips; a good
    /// hash sits near 0.5. Guards against a regression to weak Fx-style mixing.
    #[test]
    fn test_avalanche_on_integer_keys() {
        let mut total = 0u32;
        let mut trials = 0u32;
        for key in 0u64..2048 {
            let base = hash_of(&key);
            for bit in 0..16 {
                total += (base ^ hash_of(&(key ^ (1 << bit)))).count_ones();
                trials += 1;
            }
        }
        let mean = f64::from(total) / f64::from(trials) / 64.0;
        assert!((0.45..0.55).contains(&mean), "avalanche mean {mean}");
    }

    #[test]
    fn test_avalanche_on_string_keys() {
        let mut total = 0u32;
        let mut trials = 0u32;
        for n in 0..512 {
            let key = format!("crate::module::function_{n}");
            let base = hash_of(key.as_bytes());
            for i in 0..key.len() {
                let mut bytes = key.clone().into_bytes();
                bytes[i] ^= 1;
                total += (base ^ hash_of(&bytes[..])).count_ones();
                trials += 1;
            }
        }
        let mean = f64::from(total) / f64::from(trials) / 64.0;
        assert!((0.45..0.55).contains(&mean), "avalanche mean {mean}");
    }

    /// Dense ids and low-entropy strings must spread across buckets in both the
    /// low bits (bucket index) and the top 7 bits (hashbrown control byte).
    #[test]
    fn test_bucket_distribution_is_uniform() {
        const BUCKETS: usize = 1024;
        let n = BUCKETS * 64;
        let mut low = vec![0u32; BUCKETS];
        let mut high = vec![0u32; 128];
        for i in 0..n {
            let h = hash_of(&(i as u64));
            low[(h as usize) & (BUCKETS - 1)] += 1;
            high[(h >> 57) as usize] += 1;
        }
        let expect_low = (n / BUCKETS) as f64;
        let expect_high = n as f64 / 128.0;
        let chi = |counts: &[u32], expect: f64| -> f64 {
            counts
                .iter()
                .map(|&c| (f64::from(c) - expect).powi(2) / expect)
                .sum::<f64>()
        };
        // Chi-square with k-1 degrees of freedom; generous 1.35x mean bound.
        assert!(chi(&low, expect_low) < 1023.0 * 1.35, "low bits skewed");
        assert!(chi(&high, expect_high) < 127.0 * 1.6, "high bits skewed");
    }

    #[test]
    fn test_map_and_set_behave_like_std() {
        let mut map: FastMap<String, u32> = fast_map_with_capacity(4);
        for i in 0..1000u32 {
            map.insert(format!("k{i}"), i);
        }
        assert_eq!(map.len(), 1000);
        assert_eq!(map.get("k777"), Some(&777));
        let mut set: FastSet<u32> = fast_set_with_capacity(4);
        assert!(set.insert(7));
        assert!(!set.insert(7));
    }

    #[test]
    fn test_bincode_format_is_hasher_independent() {
        let mut fast: FastMap<String, u32> = FastMap::default();
        fast.insert("only".to_string(), 1);
        let mut std_map: HashMap<String, u32> = HashMap::new();
        std_map.insert("only".to_string(), 1);
        let a = bincode::serialize(&fast).unwrap();
        let b = bincode::serialize(&std_map).unwrap();
        assert_eq!(a, b);
        let back: FastMap<String, u32> = bincode::deserialize(&b).unwrap();
        assert_eq!(back, fast);
    }
}
