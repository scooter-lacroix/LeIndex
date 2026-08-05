//! Generation manifest format: types, serialization, validation.
//!
//! A manifest is the metadata header of an immutable generation. It records:
//!
//! - The format magic (`LIDX-GEN1`) and version for forward/backward detection.
//! - The generation number (monotonically increasing).
//! - Model identity (name + digest + dimensions) for model-derived layers.
//! - Graph and search fingerprints (blake3 hashes) for integrity validation.
//! - A layer-to-CAS-hash map linking each [`LayerKind`] to its blob hash.
//!
//! ## On-disk layout
//!
//! ```text
//! [MANIFEST_MAGIC 8B] [bincode(Manifest)]
//! ```
//!
//! The magic is checked byte-by-byte before any parsing occurs. The version
//! field lives inside the bincode-encoded struct and is checked immediately
//! after deserialization.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Magic bytes identifying a LeIndex generation manifest (`LIDX-GEN1`).
pub const MANIFEST_MAGIC: &[u8; 9] = b"LIDX-GEN1";

/// Current manifest format version. Bumped when the on-disk layout changes
/// in a way that older readers cannot safely ignore.
pub const MANIFEST_VERSION: u16 = 1;

// ---------------------------------------------------------------------------
// LayerKind
// ---------------------------------------------------------------------------

/// The five layer kinds that make up a complete generation. Each layer is
/// stored as an independent CAS blob; the manifest records the mapping from
/// `LayerKind` to blake3 hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayerKind {
    /// SQLite database layer (symbols, metadata, full-text index).
    Db,
    /// TF-IDF sparse/dense matrix layer.
    Tfidf,
    /// Neural embedding vectors layer.
    Neural,
    /// Program Dependence Graph layer.
    Pdg,
    /// Extracted symbol table layer.
    Symbols,
}

/// All known layer kinds in canonical order.
pub const ALL_LAYER_KINDS: [LayerKind; 5] = [
    LayerKind::Db,
    LayerKind::Tfidf,
    LayerKind::Neural,
    LayerKind::Pdg,
    LayerKind::Symbols,
];

// ---------------------------------------------------------------------------
// ModelIdentity
// ---------------------------------------------------------------------------

/// Identity of the model used to produce the neural layer. Stored in the
/// manifest so that readers can detect model mismatches and refuse to serve
/// mixed-model data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    /// Human-readable model name (e.g. `all-MiniLM-L6-v2`).
    pub name: String,
    /// Content digest of the model weights/config (e.g. `sha256:...`).
    pub digest: String,
    /// Output embedding dimensionality.
    pub dimensions: u32,
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// The full manifest stored alongside each generation.
///
/// Maps each [`LayerKind`] to a blake3 CAS blob hash, plus carries integrity
/// fingerprints and model identity. Serialised with bincode for compactness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (currently [`MANIFEST_VERSION`]).
    pub version: u16,
    /// Monotonically increasing generation number.
    pub generation: u64,
    /// Model identity for the neural layer.
    pub model_identity: ModelIdentity,
    /// blake3 hash of the graph layer data (PDG + symbols). Recomputed on
    /// read and compared against this stored value to detect corruption.
    pub graph_fingerprint: [u8; 32],
    /// blake3 hash of the search layer data (TF-IDF + neural vectors).
    /// Recomputed on read and compared against this stored value.
    pub search_fingerprint: [u8; 32],
    /// Layer-kind to CAS blob hash mapping. A valid manifest has exactly
    /// [`ALL_LAYER_KINDS.len()`] entries, one per layer.
    pub layers: HashMap<LayerKind, [u8; 32]>,
}

/// Subset of [`Manifest`] used for serialization after the magic header.
/// Not used directly — `Manifest` serializes the whole struct via bincode.
pub type ManifestBody = Manifest;

impl Manifest {
    /// Serialize this manifest to the on-disk byte format.
    ///
    /// Layout: `MANIFEST_MAGIC(9)` + `bincode(&self)`.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ManifestError> {
        let body = bincode::serialize(self).map_err(|e| ManifestError::Serialize(e.to_string()))?;
        let mut out = Vec::with_capacity(MANIFEST_MAGIC.len() + body.len());
        out.extend_from_slice(MANIFEST_MAGIC);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Deserialize a manifest from the on-disk byte format.
    ///
    /// Checks magic first, then bincode-deserializes the body, then validates
    /// version and layer completeness.
    pub fn from_bytes(data: &[u8]) -> Result<Self, ManifestError> {
        if data.len() < MANIFEST_MAGIC.len() {
            return Err(ManifestError::Truncated {
                min: MANIFEST_MAGIC.len(),
                actual: data.len(),
            });
        }
        if &data[0..MANIFEST_MAGIC.len()] != MANIFEST_MAGIC {
            let mut bad = [0u8; 9];
            bad.copy_from_slice(&data[0..MANIFEST_MAGIC.len()]);
            return Err(ManifestError::BadMagic(bad));
        }
        let manifest: Manifest = bincode::deserialize(&data[MANIFEST_MAGIC.len()..])
            .map_err(|e| ManifestError::Deserialize(e.to_string()))?;
        if manifest.version == 0 {
            return Err(ManifestError::UnsupportedVersion {
                got: 0,
                expected: MANIFEST_VERSION,
            });
        }
        if manifest.version > MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                got: manifest.version,
                expected: MANIFEST_VERSION,
            });
        }
        manifest.validate_layers()?;
        Ok(manifest)
    }

    /// Ensure all required layers are present. Returns `Err` if any
    /// [`LayerKind`] is missing from `self.layers`.
    pub fn validate_layers(&self) -> Result<(), ManifestError> {
        for kind in ALL_LAYER_KINDS.iter() {
            if !self.layers.contains_key(kind) {
                return Err(ManifestError::MissingLayer(*kind));
            }
        }
        Ok(())
    }

    /// Validate the graph fingerprint against a recomputed value.
    ///
    /// A mismatch indicates corruption or tampering. The reader must not
    /// serve data from a generation whose fingerprints do not match.
    pub fn validate_graph_fingerprint(&self, recomputed: &[u8; 32]) -> Result<(), ManifestError> {
        if &self.graph_fingerprint != recomputed {
            return Err(ManifestError::GraphFingerprintMismatch {
                stored: self.graph_fingerprint,
                recomputed: *recomputed,
            });
        }
        Ok(())
    }

    /// Validate the search fingerprint against a recomputed value.
    pub fn validate_search_fingerprint(&self, recomputed: &[u8; 32]) -> Result<(), ManifestError> {
        if &self.search_fingerprint != recomputed {
            return Err(ManifestError::SearchFingerprintMismatch {
                stored: self.search_fingerprint,
                recomputed: *recomputed,
            });
        }
        Ok(())
    }

    /// All CAS blob hashes referenced by this manifest's layers.
    ///
    /// Used by [`GenerationLease`](super::lease::GenerationLease) to increment
    /// and decrement refcounts.
    pub fn layer_hashes(&self) -> Vec<[u8; 32]> {
        // Return in canonical order for deterministic testing.
        ALL_LAYER_KINDS
            .iter()
            .filter_map(|k| self.layers.get(k).copied())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors arising during manifest parsing, validation, or fingerprint checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    /// The byte buffer is too short to contain even the magic header.
    #[error("manifest truncated: {actual} bytes, need at least {min}")]
    Truncated {
        /// Minimum required size.
        min: usize,
        /// Actual bytes available.
        actual: usize,
    },
    /// The magic bytes do not match `LIDX-GEN1`.
    #[error("bad manifest magic: got {0:?}")]
    BadMagic([u8; 9]),
    /// The manifest version is not supported by this reader.
    #[error("unsupported manifest version: got {got}, expected {expected}")]
    UnsupportedVersion {
        /// Version found on disk.
        got: u16,
        /// Version this reader expects.
        expected: u16,
    },
    /// A required layer kind is missing from the manifest.
    #[error("manifest missing required layer: {0}")]
    MissingLayer(LayerKind),
    /// bincode deserialization failure.
    #[error("manifest deserialize error: {0}")]
    Deserialize(String),
    /// bincode serialization failure.
    #[error("manifest serialize error: {0}")]
    Serialize(String),
    /// The manifest file for a generation does not exist on disk.
    #[error("manifest not found for generation {generation}: {path}")]
    MissingManifest {
        /// Generation number whose manifest was requested.
        generation: u64,
        /// Filesystem path that was checked.
        path: String,
    },
    /// The recomputed graph fingerprint does not match the stored value.
    #[error("graph fingerprint mismatch: stored {stored:?}, recomputed {recomputed:?}")]
    GraphFingerprintMismatch {
        /// Fingerprint from the manifest.
        stored: [u8; 32],
        /// Fingerprint recomputed from loaded data.
        recomputed: [u8; 32],
    },
    /// The recomputed search fingerprint does not match the stored value.
    #[error("search fingerprint mismatch: stored {stored:?}, recomputed {recomputed:?}")]
    SearchFingerprintMismatch {
        /// Fingerprint from the manifest.
        stored: [u8; 32],
        /// Fingerprint recomputed from loaded data.
        recomputed: [u8; 32],
    },
}

impl fmt::Display for LayerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayerKind::Db => write!(f, "db"),
            LayerKind::Tfidf => write!(f, "tfidf"),
            LayerKind::Neural => write!(f, "neural"),
            LayerKind::Pdg => write!(f, "pdg"),
            LayerKind::Symbols => write!(f, "symbols"),
        }
    }
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
