//! Trigram extraction for the text index.
//!
//! Trigrams are taken over raw bytes with ASCII case folded, so one index
//! serves both case-sensitive and case-insensitive queries (the verification
//! pass applies the exact case rule). Non-ASCII bytes are kept as-is, which
//! makes UTF-8 text searchable byte-wise without a decode step.

/// Three folded bytes packed into the low 24 bits.
pub type Trigram = u32;

/// Number of distinct trigram values (`2^24`).
pub const TRIGRAM_SPACE: usize = 1 << 24;

const fn fold_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = if i >= b'A' as usize && i <= b'Z' as usize {
            (i as u8) + 32
        } else {
            i as u8
        };
        i += 1;
    }
    table
}

static FOLD: [u8; 256] = fold_table();

/// Pack three bytes into a case-folded trigram.
#[inline]
pub fn pack(a: u8, b: u8, c: u8) -> Trigram {
    (u32::from(FOLD[a as usize]) << 16)
        | (u32::from(FOLD[b as usize]) << 8)
        | u32::from(FOLD[c as usize])
}

/// Distinct trigrams of a query literal, sorted.
pub fn literal_trigrams(bytes: &[u8]) -> Vec<Trigram> {
    let mut out: Vec<Trigram> = bytes.windows(3).map(|w| pack(w[0], w[1], w[2])).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Reusable distinct-trigram extractor.
///
/// A 2 MiB presence bitset makes extraction O(bytes) with no hashing, and only
/// the bits that were set are cleared afterwards, so per-file cost does not
/// depend on the bitset size. Keep one per worker thread.
pub struct Extractor {
    seen: Vec<u64>,
    touched: Vec<Trigram>,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    /// Allocate the presence bitset.
    pub fn new() -> Self {
        Self {
            seen: vec![0; TRIGRAM_SPACE / 64],
            touched: Vec::new(),
        }
    }

    /// Distinct trigrams in `data`, sorted ascending.
    pub fn distinct(&mut self, data: &[u8]) -> Vec<Trigram> {
        self.touched.clear();
        for w in data.windows(3) {
            let t = pack(w[0], w[1], w[2]);
            let (word, bit) = ((t >> 6) as usize, 1u64 << (t & 63));
            if self.seen[word] & bit == 0 {
                self.seen[word] |= bit;
                self.touched.push(t);
            }
        }
        for &t in &self.touched {
            self.seen[(t >> 6) as usize] &= !(1u64 << (t & 63));
        }
        let mut out = self.touched.clone();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pack_folds_ascii_case_only() {
        assert_eq!(pack(b'A', b'b', b'C'), pack(b'a', b'B', b'c'));
        assert_ne!(pack(0xC3, 0xA9, b'x'), pack(0xC3, 0x89, b'x'));
    }

    #[test]
    fn test_literal_trigrams_are_distinct_and_sorted() {
        let t = literal_trigrams(b"aaaa");
        assert_eq!(t.len(), 1);
        let t = literal_trigrams(b"abcabc");
        assert_eq!(t.len(), 3);
        assert!(t.windows(2).all(|w| w[0] < w[1]));
        assert!(literal_trigrams(b"ab").is_empty());
    }

    #[test]
    fn test_extractor_is_reusable_and_matches_the_reference() {
        let mut extractor = Extractor::new();
        let first = extractor.distinct(b"Hello, hello world");
        assert_eq!(first, literal_trigrams(b"Hello, hello world"));
        // A second call must not see bits left over from the first.
        let second = extractor.distinct(b"xyz");
        assert_eq!(second, literal_trigrams(b"xyz"));
    }
}
