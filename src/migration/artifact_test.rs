use super::*;
use crate::storage::cas::blob::{self, BLOB_MAGIC, BLOB_VERSION};
use crate::storage::generation::manifest::{
    LayerKind, MANIFEST_MAGIC, MANIFEST_VERSION, Manifest, ModelIdentity,
};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

fn dummy_model_identity() -> ModelIdentity {
    ModelIdentity {
        name: "all-MiniLM-L6-v2".to_string(),
        digest: "sha256:abc123".to_string(),
        dimensions: 384,
    }
}

fn dummy_manifest() -> Manifest {
    let mut layers = HashMap::new();
    let mut hash_seed = 0u8;
    for kind in [
        LayerKind::Db,
        LayerKind::Tfidf,
        LayerKind::Neural,
        LayerKind::Pdg,
        LayerKind::Symbols,
    ] {
        hash_seed += 1;
        layers.insert(kind, [hash_seed; 32]);
    }
    Manifest {
        version: MANIFEST_VERSION,
        generation: 1,
        model_identity: dummy_model_identity(),
        graph_fingerprint: [0xAA; 32],
        search_fingerprint: [0xBB; 32],
        layers,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// VAL-ROLLOUT-002: CAS blob magic/version/checksum validation
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_blob_valid_magic_version_checksum() {
    let payload = b"hello world";
    let blob_bytes = blob::encode_blob(payload);
    let hash = validate_blob(&blob_bytes).expect("valid blob should pass");
    assert_eq!(hash, blob::blob_hash(payload));
}

#[test]
fn test_blob_rejects_bad_magic() {
    let payload = b"hello world";
    let mut blob_bytes = blob::encode_blob(payload);
    // Corrupt the magic.
    blob_bytes[0] = b'X';
    let err = validate_blob(&blob_bytes).unwrap_err();
    assert!(
        err.to_string().contains("magic"),
        "error should mention magic: {}",
        err
    );
}

#[test]
fn test_blob_rejects_bad_version() {
    let payload = b"hello world";
    let mut blob_bytes = blob::encode_blob(payload);
    // Corrupt the version byte.
    blob_bytes[9] = 255;
    let err = validate_blob(&blob_bytes).unwrap_err();
    match err {
        ArtifactError::BadBlob(BadBlob::VersionMismatch { got, expected }) => {
            assert_eq!(got, 255);
            assert_eq!(expected, BLOB_VERSION);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn test_blob_rejects_hash_mismatch() {
    let payload = b"hello world";
    let mut blob_bytes = blob::encode_blob(payload);
    // Flip a payload byte to break the blake3 checksum.
    let payload_start = blob::BLOB_HEADER_LEN;
    blob_bytes[payload_start] ^= 0xFF;
    let err = validate_blob(&blob_bytes).unwrap_err();
    match err {
        ArtifactError::BadBlob(BadBlob::HashMismatch) => {}
        other => panic!("expected HashMismatch, got {other:?}"),
    }
}

#[test]
fn test_blob_rejects_truncated() {
    let truncated = b"LIDX-BL";
    let err = validate_blob(truncated).unwrap_err();
    assert!(matches!(
        err,
        ArtifactError::BadBlob(BadBlob::Truncated { .. })
    ));
}

#[test]
fn test_blob_format_identity() {
    let (magic, version) = blob_format_identity();
    assert_eq!(magic, BLOB_MAGIC);
    assert_eq!(version, BLOB_VERSION);
}

#[test]
fn test_blob_extract_payload_roundtrip() {
    let payload = b"\x00\x01\x02\x03large binary payload\xFF";
    let blob_bytes = blob::encode_blob(payload);
    let (extracted, hash) = extract_blob_payload(&blob_bytes).expect("extraction succeeds");
    assert_eq!(extracted, payload);
    assert_eq!(hash, blob::blob_hash(payload));
}

// ─────────────────────────────────────────────────────────────────────────
// VAL-ROLLOUT-003: Generation manifest magic/version/checksum validation
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_manifest_valid_magic_version() {
    let manifest = dummy_manifest();
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let validated = validate_manifest(&bytes).expect("valid manifest passes");
    assert_eq!(validated.generation, 1);
    assert_eq!(validated.model_identity.name, "all-MiniLM-L6-v2");
}

#[test]
fn test_manifest_rejects_bad_magic() {
    let manifest = dummy_manifest();
    let mut bytes = manifest.to_bytes().expect("serialization succeeds");
    // Corrupt the magic.
    bytes[0] = b'X';
    let err = validate_manifest(&bytes).unwrap_err();
    assert!(
        err.to_string().contains("magic"),
        "error should mention magic: {}",
        err
    );
}

#[test]
fn test_manifest_rejects_unsupported_version() {
    let mut manifest = dummy_manifest();
    manifest.version = 999;
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let err = validate_manifest(&bytes).unwrap_err();
    assert!(
        err.to_string().contains("version"),
        "error should mention version: {}",
        err
    );
}

#[test]
fn test_manifest_rejects_version_zero() {
    let mut manifest = dummy_manifest();
    manifest.version = 0;
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let err = validate_manifest(&bytes).unwrap_err();
    assert!(
        err.to_string().contains("version"),
        "error should mention version: {}",
        err
    );
}

#[test]
fn test_manifest_rejects_missing_layer() {
    let mut manifest = dummy_manifest();
    manifest.layers.remove(&LayerKind::Neural);
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let err = validate_manifest(&bytes).unwrap_err();
    assert!(
        err.to_string().contains("layer"),
        "error should mention missing layer: {}",
        err
    );
}

#[test]
fn test_manifest_format_identity() {
    let (magic, version) = manifest_format_identity();
    assert_eq!(magic, MANIFEST_MAGIC);
    assert_eq!(version, MANIFEST_VERSION);
}

#[test]
fn test_manifest_rejects_wrong_magic_bytes() {
    // Construct bytes with intact magic area but wrong values.
    let mut bad = vec![0u8; 16];
    bad[0..9].copy_from_slice(b"LIDX-GEN0"); // wrong version suffix
    let err = validate_manifest(&bad).unwrap_err();
    assert!(matches!(err, ArtifactError::BadManifest(_)));
}

#[test]
fn test_manifest_fingerprint_validation_pass() {
    let manifest = dummy_manifest();
    let graph = manifest.graph_fingerprint;
    let search = manifest.search_fingerprint;
    validate_manifest_fingerprints(&manifest, &graph, &search)
        .expect("matching fingerprints should pass");
}

#[test]
fn test_manifest_fingerprint_validation_fail_graph() {
    let manifest = dummy_manifest();
    let wrong = [0x00; 32];
    let err = validate_manifest_fingerprints(&manifest, &wrong, &manifest.search_fingerprint)
        .unwrap_err();
    assert!(err.to_string().contains("graph"));
}

#[test]
fn test_manifest_fingerprint_validation_fail_search() {
    let manifest = dummy_manifest();
    let wrong = [0x00; 32];
    let err =
        validate_manifest_fingerprints(&manifest, &manifest.graph_fingerprint, &wrong).unwrap_err();
    assert!(err.to_string().contains("search"));
}

// ─────────────────────────────────────────────────────────────────────────
// VAL-ROLLOUT-004: Model/vector identity mismatch forces rebuild
// ─────────────────────────────────────────────────────────────────────────

#[test]
fn test_model_identity_match_returns_ok() {
    let manifest = dummy_manifest();
    let expected = dummy_model_identity();
    let outcome = check_model_identity(&manifest, &expected);
    assert_eq!(outcome, ArtifactOutcome::Ok(()));
}

#[test]
fn test_model_name_mismatch_triggers_rebuild() {
    let manifest = dummy_manifest();
    let mut expected = dummy_model_identity();
    expected.name = "CodeRankEmbed".to_string();
    match check_model_identity(&manifest, &expected) {
        ArtifactOutcome::Rebuild(RebuildReason::ModelNameMismatch {
            expected: e,
            actual: a,
        }) => {
            assert_eq!(e, "CodeRankEmbed");
            assert_eq!(a, "all-MiniLM-L6-v2");
        }
        other => panic!("expected ModelNameMismatch rebuild, got {other:?}"),
    }
}

#[test]
fn test_model_digest_mismatch_triggers_rebuild() {
    let manifest = dummy_manifest();
    let mut expected = dummy_model_identity();
    expected.digest = "sha256:different".to_string();
    match check_model_identity(&manifest, &expected) {
        ArtifactOutcome::Rebuild(RebuildReason::ModelDigestMismatch {
            expected: e,
            actual: a,
        }) => {
            assert_eq!(e, "sha256:different");
            assert_eq!(a, "sha256:abc123");
        }
        other => panic!("expected ModelDigestMismatch rebuild, got {other:?}"),
    }
}

#[test]
fn test_model_dimension_mismatch_triggers_rebuild() {
    let manifest = dummy_manifest();
    let mut expected = dummy_model_identity();
    expected.dimensions = 1024;
    match check_model_identity(&manifest, &expected) {
        ArtifactOutcome::Rebuild(RebuildReason::ModelDimensionMismatch {
            expected: e,
            actual: a,
        }) => {
            assert_eq!(e, 1024);
            assert_eq!(a, 384);
        }
        other => panic!("expected ModelDimensionMismatch rebuild, got {other:?}"),
    }
}

#[test]
fn test_rebuild_reason_actionable_message_contains_remedy() {
    let reason = RebuildReason::ModelNameMismatch {
        expected: "Model-A".to_string(),
        actual: "Model-B".to_string(),
    };
    let msg = reason.actionable_message();
    assert!(msg.contains("leindex index --force"));
    assert!(msg.contains("Model-A"));
    assert!(msg.contains("Model-B"));
}

#[test]
fn test_validate_generation_manifest_with_valid_identity() {
    let manifest = dummy_manifest();
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let expected = dummy_model_identity();
    let outcome = validate_generation_manifest(&bytes, Some(&expected)).expect("format ok");
    match outcome {
        ArtifactOutcome::Ok(m) => assert_eq!(m.generation, 1),
        ArtifactOutcome::Rebuild(r) => panic!("should not rebuild: {r}"),
    }
}

#[test]
fn test_validate_generation_manifest_with_mismatched_identity() {
    let manifest = dummy_manifest();
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let mut expected = dummy_model_identity();
    expected.dimensions = 768;
    let outcome = validate_generation_manifest(&bytes, Some(&expected)).expect("format ok");
    match outcome {
        ArtifactOutcome::Ok(_) => panic!("should have triggered rebuild"),
        ArtifactOutcome::Rebuild(RebuildReason::ModelDimensionMismatch { .. }) => {}
        other => panic!("expected DimensionMismatch rebuild, got {other:?}"),
    }
}

#[test]
fn test_validate_generation_manifest_without_model_check() {
    let manifest = dummy_manifest();
    let bytes = manifest.to_bytes().expect("serialization succeeds");
    let outcome = validate_generation_manifest(&bytes, None).expect("format ok");
    match outcome {
        ArtifactOutcome::Ok(m) => assert_eq!(m.layers.len(), 5),
        ArtifactOutcome::Rebuild(r) => panic!("should not rebuild without model check: {r}"),
    }
}
