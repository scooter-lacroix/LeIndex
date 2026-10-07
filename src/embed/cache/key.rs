//! CacheKey — the spec §6.5 6-tuple that identifies a cached embedding.
//!
//! The key is a content-addressed identity comprising model identity, pooling
//! and normalization parameters, output dimensions, and a blake3 hash of the
//! source text. Two texts with the same content hash from different projects
//! produce the same key (cross-project dedup, spec §10.1). A model upgrade
//! changes the model digest, which changes the fingerprint, creating a new
//! namespace (spec §10.1 — no accidental mixed vectors).

use blake3::Hasher;
use serde::{Deserialize, Serialize};

/// Pooling strategy applied to the model output.
///
/// Different pooling strategies produce different vectors for the same input,
/// so pooling is part of the cache key identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pooling {
    /// CLS token pooling.
    Cls,
    /// Mean token pooling.
    Mean,
    /// Max token pooling.
    Max,
    /// Last token pooling.
    LastToken,
}

impl Pooling {
    /// Encode this variant as a stable discriminator byte for fingerprinting.
    fn discriminant_byte(&self) -> u8 {
        match self {
            Self::Cls => 0,
            Self::Mean => 1,
            Self::Max => 2,
            Self::LastToken => 3,
        }
    }
}

/// Normalization applied to the output vector.
///
/// Normalized and unnormalized vectors are not interchangeable, so this is
/// part of the cache key identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    /// No normalization applied.
    None,
    /// L2 normalization.
    L2,
}

impl Normalization {
    /// Encode this variant as a stable discriminator byte for fingerprinting.
    fn discriminant_byte(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::L2 => 1,
        }
    }
}

/// Content-addressed cache key for a single embedding (spec §6.5).
///
/// The key is a 6-field tuple:
/// - `model_digest` — blake3 of the ONNX model bytes
/// - `tokenizer_digest` — blake3 of the tokenizer config bytes
/// - `prompt_role_and_version` — prompt template role + version packed into a u32
/// - `pooling` — pooling strategy
/// - `normalization` — normalization mode
/// - `output_dimensions` — number of output dimensions
/// - `content_hash` — blake3 of the source text
///
/// Any field differing produces a different fingerprint. Identical source
/// text from different projects produces the same `content_hash`, enabling
/// cross-project deduplication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// Blake3 digest of the ONNX model weights.
    pub model_digest: [u8; 32],
    /// Blake3 digest of the tokenizer configuration.
    pub tokenizer_digest: [u8; 32],
    /// Prompt template role and version packed into a u32.
    pub prompt_role_and_version: u32,
    /// Pooling strategy.
    pub pooling: Pooling,
    /// Normalization mode.
    pub normalization: Normalization,
    /// Number of output dimensions.
    pub output_dimensions: u32,
    /// Blake3 hash of the source text.
    pub content_hash: [u8; 32],
}

impl CacheKey {
    /// Compute the 32-byte fingerprint of this cache key.
    ///
    /// The fingerprint is a blake3 hash over a canonical encoding of all 7
    /// fields. It is the namespace key for the global embedding cache:
    /// any field differing produces a different fingerprint, and identical
    /// fields always produce the same fingerprint.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = Hasher::new();
        // Deterministic ordering: each field is length-prefixed conceptually
        // by its fixed size so there is no ambiguity in the encoding.
        hasher.update(b"LEINDEX-CACHEKEY-V1");
        hasher.update(&self.model_digest);
        hasher.update(&self.tokenizer_digest);
        hasher.update(&self.prompt_role_and_version.to_le_bytes());
        hasher.update(&[self.pooling.discriminant_byte()]);
        hasher.update(&[self.normalization.discriminant_byte()]);
        hasher.update(&self.output_dimensions.to_le_bytes());
        hasher.update(&self.content_hash);
        hasher.finalize().into()
    }

    /// Compute the blake3 model digest from raw model bytes.
    pub fn model_digest(model_bytes: &[u8]) -> [u8; 32] {
        blake3::hash(model_bytes).into()
    }

    /// Compute the blake3 tokenizer digest from tokenizer config bytes.
    pub fn tokenizer_digest(tokenizer_bytes: &[u8]) -> [u8; 32] {
        blake3::hash(tokenizer_bytes).into()
    }

    /// Compute the blake3 content hash from source text.
    pub fn content_hash(text: &str) -> [u8; 32] {
        blake3::hash(text.as_bytes()).into()
    }

    /// Pack a prompt role (lower 16 bits) and version (upper 16 bits) into a
    /// single u32.
    pub fn pack_prompt_role_and_version(role: u16, version: u16) -> u32 {
        (u32::from(version) << 16) | u32::from(role)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_key(content: &str) -> CacheKey {
        CacheKey {
            model_digest: CacheKey::model_digest(b"model-bytes-v1"),
            tokenizer_digest: CacheKey::tokenizer_digest(b"tokenizer-config"),
            prompt_role_and_version: CacheKey::pack_prompt_role_and_version(1, 1),
            pooling: Pooling::Mean,
            normalization: Normalization::L2,
            output_dimensions: 1024,
            content_hash: CacheKey::content_hash(content),
        }
    }

    /// VAL-CACHE-001: Same model+content = same key.
    #[test]
    fn test_same_fields_produce_same_fingerprint() {
        let key_a = sample_key("hello world");
        let key_b = sample_key("hello world");
        assert_eq!(key_a.fingerprint(), key_b.fingerprint());
    }

    /// VAL-CACHE-001: Any single field differing produces a different fingerprint.
    #[test]
    fn test_model_digest_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.model_digest = CacheKey::model_digest(b"different-model");
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    #[test]
    fn test_tokenizer_digest_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.tokenizer_digest = CacheKey::tokenizer_digest(b"different-tokenizer");
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    #[test]
    fn test_prompt_role_and_version_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.prompt_role_and_version = CacheKey::pack_prompt_role_and_version(2, 1);
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());

        let mut key_c = sample_key("hello");
        key_c.prompt_role_and_version = CacheKey::pack_prompt_role_and_version(1, 2);
        assert_ne!(key_a.fingerprint(), key_c.fingerprint());
    }

    #[test]
    fn test_pooling_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.pooling = Pooling::Cls;
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    #[test]
    fn test_normalization_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.normalization = Normalization::None;
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    #[test]
    fn test_output_dimensions_differ() {
        let key_a = sample_key("hello");
        let mut key_b = key_a.clone();
        key_b.output_dimensions = 768;
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    #[test]
    fn test_content_hash_differ() {
        let key_a = sample_key("hello world");
        let key_b = sample_key("goodbye world");
        assert_ne!(key_a.fingerprint(), key_b.fingerprint());
    }

    /// VAL-CACHE-001: Identical source text from different "projects" produces
    /// the same content_hash (cross-project dedup).
    #[test]
    fn test_content_hash_same_across_projects() {
        let text = "fn main() { println!(\"hello\"); }";
        let hash_a = CacheKey::content_hash(text);
        let hash_b = CacheKey::content_hash(text);
        assert_eq!(hash_a, hash_b);

        // The full CacheKey for the same text + model is identical regardless
        // of which project produced it.
        let key_a = sample_key(text);
        let key_b = sample_key(text);
        assert_eq!(key_a.fingerprint(), key_b.fingerprint());
    }

    /// VAL-CACHE-002: Model upgrade creates new namespace.
    #[test]
    fn test_model_upgrade_new_namespace() {
        let key_v1 = CacheKey {
            model_digest: CacheKey::model_digest(b"model-v1-bytes"),
            tokenizer_digest: CacheKey::tokenizer_digest(b"tok"),
            prompt_role_and_version: 0,
            pooling: Pooling::Mean,
            normalization: Normalization::L2,
            output_dimensions: 1024,
            content_hash: CacheKey::content_hash("same text"),
        };

        let key_v2 = CacheKey {
            model_digest: CacheKey::model_digest(b"model-v2-bytes"),
            ..key_v1.clone()
        };

        assert_ne!(
            key_v1.fingerprint(),
            key_v2.fingerprint(),
            "model upgrade must produce a new namespace"
        );
    }

    /// fingerprint() returns [u8; 32].
    #[test]
    fn test_fingerprint_is_32_bytes() {
        let key = sample_key("test");
        let fp = key.fingerprint();
        assert_eq!(fp.len(), 32);
    }

    #[test]
    fn test_pack_prompt_role_and_version() {
        let packed = CacheKey::pack_prompt_role_and_version(5, 3);
        assert_eq!(packed & 0xFFFF, 5);
        assert_eq!(packed >> 16, 3);
    }

    #[test]
    fn test_all_pooling_variants_distinct_fingerprints() {
        let base = sample_key("hello");
        let poolings = [
            Pooling::Cls,
            Pooling::Mean,
            Pooling::Max,
            Pooling::LastToken,
        ];
        let mut fingerprints = std::collections::HashSet::new();
        for p in &poolings {
            let mut key = base.clone();
            key.pooling = *p;
            fingerprints.insert(key.fingerprint());
        }
        assert_eq!(
            fingerprints.len(),
            poolings.len(),
            "all pooling variants must produce distinct fingerprints"
        );
    }

    #[test]
    fn test_all_normalization_variants_distinct_fingerprints() {
        let base = sample_key("hello");
        let norms = [Normalization::None, Normalization::L2];
        let mut fingerprints = std::collections::HashSet::new();
        for n in &norms {
            let mut key = base.clone();
            key.normalization = *n;
            fingerprints.insert(key.fingerprint());
        }
        assert_eq!(
            fingerprints.len(),
            norms.len(),
            "all normalization variants must produce distinct fingerprints"
        );
    }
}
