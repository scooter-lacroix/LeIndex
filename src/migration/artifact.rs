//! Artifact format versioning and validation (spec §12.2).
//!
//! Every new persistent format in the v2.0.0 generation store carries a magic
//! header, a version number, and a checksum (blake3). This module provides a
//! unified validation surface that fronts the existing CAS blob and generation
//! manifest validation code, plus extends it with **model/vector identity
//! mismatch** detection: when the model/tokenizer/version in a generation
//! manifest does not match the currently configured profile, the reader
//! triggers a rebuild rather than silently serving incompatible vectors.
//!
//! ## Two Formats
//!
//! | Format | Magic | Location | Purpose |
//! |--------|-------|----------|---------|
//! | CAS blob | `LIDX-BLB1` | `cas/<prefix>/<hash>` | Self-describing content-addressed blob frame |
//! | Generation manifest | `LIDX-GEN1` | `generations/<N>/manifest` | Layer-to-blob mapping + model identity |
//!
//! ## Magic / Version / Checksum Contract
//!
//! The reader validates magic before parsing any version or layer data. A
//! mismatched magic or version is rejected with an actionable error message
//! that names the file, the expected and actual values, and the user-visible
//! remedy (trigger a rebuild). The checksum (blake3) is recomputed on read
//! for CAS blobs and for generation fingerprints.
//!
//! ## Model Identity Mismatch → Rebuild (never silent reuse)
//!
//! Spec §12.2: when the current generation's manifest records a model
//! identity (name, digest, dimensions) that does not match the current embed
//! worker profile, the reader refuses to serve neural vectors from that
//! generation. Instead, a rebuild is signalled via [`ArtifactOutcome::Rebuild`].
//! This prevents silent quality regressions when the deployed model changes.

use crate::storage::cas::blob::{self, BLOB_MAGIC, BLOB_VERSION, BadBlob};
use crate::storage::generation::manifest::{
    MANIFEST_MAGIC, MANIFEST_VERSION, Manifest, ManifestError, ModelIdentity,
};

/// Outcome of validating an artifact against expected identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactOutcome<T> {
    /// The artifact is valid and matches expectations.
    Ok(T),
    /// The artifact's format is parseable but its model/vector identity does
    /// not match the expected profile. The caller must trigger a rebuild.
    /// This is never a silent fallback: the caller knows vectors are stale.
    Rebuild(RebuildReason),
}

/// Exact reason a rebuild was forced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuildReason {
    /// The model name differs (e.g., switched from `all-MiniLM-L6-v2` to
    /// `CodeRankEmbed`).
    ModelNameMismatch {
        /// Expected model name from the current profile.
        expected: String,
        /// Actual model name from the generation manifest.
        actual: String,
    },
    /// The model digest (weights/config hash) differs.
    ModelDigestMismatch {
        /// Expected model digest from the current profile.
        expected: String,
        /// Actual model digest from the generation manifest.
        actual: String,
    },
    /// The embedding dimensionality differs (e.g., 384 → 1024).
    ModelDimensionMismatch {
        /// Expected dimensions from the current profile.
        expected: u32,
        /// Actual dimensions from the generation manifest.
        actual: u32,
    },
}

impl RebuildReason {
    /// Returns a user-visible actionable message explaining the mismatch.
    pub fn actionable_message(&self) -> String {
        match self {
            Self::ModelNameMismatch { expected, actual } => {
                format!(
                    "Model name mismatch: manifest says '{actual}' but current profile is \
                     '{expected}'. A rebuild is required to re-embed with the new model. \
                     Run: leindex index --force"
                )
            }
            Self::ModelDigestMismatch { expected, actual } => {
                format!(
                    "Model digest mismatch: manifest says '{actual}' but current model digest is \
                     '{expected}'. The model weights/tokenizer have changed. \
                     Run: leindex index --force to rebuild neural vectors."
                )
            }
            Self::ModelDimensionMismatch { expected, actual } => {
                format!(
                    "Embedding dimension mismatch: manifest says {actual} but current model \
                     produces {expected} dimensions. Run: leindex index --force to rebuild."
                )
            }
        }
    }
}

impl std::fmt::Display for RebuildReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.actionable_message())
    }
}

/// Unified artifact validation error (format-level failures).
///
/// Unlike [`RebuildReason`] (which signals a rebuild), these errors indicate
/// corruption or unsupported format and are hard failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// CAS blob validation failure (magic/version/checksum).
    #[error("CAS blob validation failed: {0}")]
    BadBlob(#[from] BadBlob),
    /// Generation manifest format failure (magic/version/layers).
    #[error("Manifest validation failed: {0}")]
    BadManifest(#[from] ManifestError),
    /// The on-disk artifact is missing entirely.
    #[error("Artifact not found: {0}")]
    NotFound(String),
}

// ─────────────────────────────────────────────────────────────────────────
// CAS blob validation (LIDX-BLB1)
// ─────────────────────────────────────────────────────────────────────────

/// Validate a CAS blob frame and return the stored blake3 hash.
///
/// Checks (in order):
/// 1. Magic bytes are `LIDX-BLB1`.
/// 2. Version byte matches [`BLOB_VERSION`].
/// 3. Payload length matches the header declaration.
/// 4. Recomputed blake3 of the payload matches the stored hash.
///
/// A failure at any step returns an actionable [`ArtifactError`].
pub fn validate_blob(bytes: &[u8]) -> Result<[u8; 32], ArtifactError> {
    blob::validate_blob(bytes).map_err(ArtifactError::BadBlob)
}

/// Validate a CAS blob and extract the payload + hash.
pub fn extract_blob_payload(bytes: &[u8]) -> Result<(&[u8], [u8; 32]), ArtifactError> {
    blob::extract_payload(bytes).map_err(ArtifactError::BadBlob)
}

/// Returns the expected magic and version for CAS blobs.
pub fn blob_format_identity() -> (&'static [u8], u8) {
    (BLOB_MAGIC, BLOB_VERSION)
}

// ─────────────────────────────────────────────────────────────────────────
// Generation manifest validation (LIDX-GEN1)
// ─────────────────────────────────────────────────────────────────────────

/// Validate a generation manifest from raw bytes.
///
/// Checks (in order):
/// 1. Magic bytes are `LIDX-GEN1`.
/// 2. Manifest version is supported (rejects 0 and > [`MANIFEST_VERSION`]).
/// 3. All five required [`LayerKind`]s are present.
///
/// Does **not** recompute graph/search fingerprints (that requires loading
/// layer data). Use [`validate_manifest_fingerprints`] for that.
pub fn validate_manifest(bytes: &[u8]) -> Result<Manifest, ArtifactError> {
    Manifest::from_bytes(bytes).map_err(ArtifactError::BadManifest)
}

/// Validate manifest fingerprints against recomputed values.
///
/// A mismatch indicates corruption or tampering. The reader must not serve
/// data from a generation whose fingerprints fail validation.
pub fn validate_manifest_fingerprints(
    manifest: &Manifest,
    graph_recomputed: &[u8; 32],
    search_recomputed: &[u8; 32],
) -> Result<(), ArtifactError> {
    manifest
        .validate_graph_fingerprint(graph_recomputed)
        .map_err(ArtifactError::BadManifest)?;
    manifest
        .validate_search_fingerprint(search_recomputed)
        .map_err(ArtifactError::BadManifest)?;
    Ok(())
}

/// Returns the expected magic and version for manifest files.
pub fn manifest_format_identity() -> (&'static [u8], u16) {
    (MANIFEST_MAGIC, MANIFEST_VERSION)
}

// ─────────────────────────────────────────────────────────────────────────
// Model/vector identity mismatch → rebuild (spec §12.2)
// ─────────────────────────────────────────────────────────────────────────

/// Check whether a generation manifest's model identity matches the currently
/// expected profile.
///
/// Returns `Ok(())` if the identity matches, or `Rebuild(reason)` if any
/// field differs. This is the spec §12.2 invariant: no mismatched-model
/// silent acceptance. The caller must trigger a full re-index to regenerate
/// neural vectors with the new model.
pub fn check_model_identity(manifest: &Manifest, expected: &ModelIdentity) -> ArtifactOutcome<()> {
    use ArtifactOutcome::*;
    if manifest.model_identity.name != expected.name {
        return Rebuild(RebuildReason::ModelNameMismatch {
            expected: expected.name.clone(),
            actual: manifest.model_identity.name.clone(),
        });
    }
    if manifest.model_identity.digest != expected.digest {
        return Rebuild(RebuildReason::ModelDigestMismatch {
            expected: expected.digest.clone(),
            actual: manifest.model_identity.digest.clone(),
        });
    }
    if manifest.model_identity.dimensions != expected.dimensions {
        return Rebuild(RebuildReason::ModelDimensionMismatch {
            expected: expected.dimensions,
            actual: manifest.model_identity.dimensions,
        });
    }
    Ok(())
}

/// Full artifact validation pipeline for a generation manifest.
///
/// This function:
/// 1. Validates magic/version/layers via [`validate_manifest`].
/// 2. Optionally checks model identity against `expected_model`.
///
/// Returns the validated manifest, or a rebuild signal, or a format error.
pub fn validate_generation_manifest(
    bytes: &[u8],
    expected_model: Option<&ModelIdentity>,
) -> Result<ArtifactOutcome<Manifest>, ArtifactError> {
    let manifest = validate_manifest(bytes)?;
    if let Some(expected) = expected_model {
        match check_model_identity(&manifest, expected) {
            ArtifactOutcome::Ok(()) => Ok(ArtifactOutcome::Ok(manifest)),
            ArtifactOutcome::Rebuild(reason) => Ok(ArtifactOutcome::Rebuild(reason)),
        }
    } else {
        Ok(ArtifactOutcome::Ok(manifest))
    }
}

#[cfg(test)]
#[path = "artifact_test.rs"]
mod tests;
