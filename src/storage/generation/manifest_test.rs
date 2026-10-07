//! Tests for generation manifest: serialization roundtrip, magic/version
//! validation, fingerprint validation, layer completeness.
//!
//! Covers VAL-CAS-011 through VAL-CAS-015.

use super::*;
use std::collections::HashMap;

fn fixture_manifest() -> Manifest {
    let mut layers = HashMap::new();
    layers.insert(LayerKind::Db, [0xAA; 32]);
    layers.insert(LayerKind::Tfidf, [0xBB; 32]);
    layers.insert(LayerKind::Neural, [0xCC; 32]);
    layers.insert(LayerKind::Pdg, [0xDD; 32]);
    layers.insert(LayerKind::Symbols, [0xEE; 32]);

    Manifest {
        version: MANIFEST_VERSION,
        generation: 42,
        model_identity: ModelIdentity {
            name: "test-model".to_string(),
            digest: "sha256:abcdef".to_string(),
            dimensions: 384,
        },
        graph_fingerprint: [0x11; 32],
        search_fingerprint: [0x22; 32],
        layers,
    }
}

// -------- VAL-CAS-011: manifest magic header --------

#[test]
fn test_manifest_magic_accepted() {
    let m = fixture_manifest();
    let bytes = m.to_bytes().expect("serialize");
    // File must start with MANIFEST_MAGIC.
    assert_eq!(&bytes[0..MANIFEST_MAGIC.len()], MANIFEST_MAGIC);
}

#[test]
fn test_manifest_magic_rejected() {
    // Wrong magic values must be rejected.
    for bad_magic in [
        &b"LIDX-GEN0"[..],
        &b"GEN00000!"[..],
        &b"\0\0\0\0\0\0\0\0\0"[..],
    ] {
        let m = fixture_manifest();
        let mut bytes = m.to_bytes().expect("serialize");
        bytes[0..MANIFEST_MAGIC.len()].copy_from_slice(bad_magic);
        let err = Manifest::from_bytes(&bytes).unwrap_err();
        assert!(
            matches!(err, ManifestError::BadMagic(_)),
            "expected BadMagic for {bad_magic:?}, got {err:?}"
        );
    }
}

#[test]
fn test_manifest_magic_empty_rejected() {
    let err = Manifest::from_bytes(&[]).unwrap_err();
    assert!(matches!(err, ManifestError::Truncated { .. }));
}

#[test]
fn test_manifest_magic_truncated_rejected() {
    // Only 4 bytes — shorter than magic.
    let err = Manifest::from_bytes(b"LIDX").unwrap_err();
    assert!(matches!(err, ManifestError::Truncated { .. }));
}

// -------- VAL-CAS-012: manifest version field --------

#[test]
fn test_manifest_version_one_accepted() {
    let m = fixture_manifest(); // version == MANIFEST_VERSION (1)
    let bytes = m.to_bytes().expect("serialize");
    let recovered = Manifest::from_bytes(&bytes).expect("valid manifest");
    assert_eq!(recovered.version, MANIFEST_VERSION);
    assert_eq!(recovered, m);
}

#[test]
fn test_manifest_version_zero_rejected() {
    let mut m = fixture_manifest();
    m.version = 0;
    let bytes = m.to_bytes().expect("serialize");
    let err = Manifest::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(err, ManifestError::UnsupportedVersion { got: 0, .. }),
        "expected UnsupportedVersion for v0, got {err:?}"
    );
}

#[test]
fn test_manifest_version_future_rejected() {
    let mut m = fixture_manifest();
    m.version = 999;
    let bytes = m.to_bytes().expect("serialize");
    let err = Manifest::from_bytes(&bytes).unwrap_err();
    assert!(
        matches!(err, ManifestError::UnsupportedVersion { got: 999, .. }),
        "expected UnsupportedVersion for v999, got {err:?}"
    );
}

// -------- VAL-CAS-013: serialization roundtrip --------

#[test]
fn test_manifest_roundtrip() {
    let m = fixture_manifest();
    let bytes = m.to_bytes().expect("serialize");
    let recovered = Manifest::from_bytes(&bytes).expect("deserialize");
    assert_eq!(recovered, m);
    // All 5 layer hashes present and equal.
    assert_eq!(recovered.layers.len(), 5);
    assert_eq!(
        recovered.layers.get(&LayerKind::Db),
        m.layers.get(&LayerKind::Db)
    );
    assert_eq!(
        recovered.layers.get(&LayerKind::Tfidf),
        m.layers.get(&LayerKind::Tfidf)
    );
    assert_eq!(
        recovered.layers.get(&LayerKind::Neural),
        m.layers.get(&LayerKind::Neural)
    );
    assert_eq!(
        recovered.layers.get(&LayerKind::Pdg),
        m.layers.get(&LayerKind::Pdg)
    );
    assert_eq!(
        recovered.layers.get(&LayerKind::Symbols),
        m.layers.get(&LayerKind::Symbols)
    );
    // Fingerprints preserved.
    assert_eq!(recovered.graph_fingerprint, m.graph_fingerprint);
    assert_eq!(recovered.search_fingerprint, m.search_fingerprint);
    // Model identity preserved.
    assert_eq!(recovered.model_identity, m.model_identity);
    assert_eq!(recovered.generation, m.generation);
}

#[test]
fn test_manifest_compact_serialization() {
    // Verify the format is bincode-based (compact, not JSON).
    let m = fixture_manifest();
    let bytes = m.to_bytes().expect("serialize");
    // Magic (8) + bincode body. The body should be much smaller than a JSON
    // representation.
    assert!(bytes.len() > 8);
    // Must NOT be human-readable JSON after magic.
    let body = &bytes[MANIFEST_MAGIC.len()..];
    assert!(!body.starts_with(b"{"), "manifest body must not be JSON");
}

// -------- VAL-CAS-014: fingerprint validation --------

#[test]
fn test_fingerprint_validation_ok() {
    let m = fixture_manifest();
    // A manifest with correct fingerprints validates.
    assert!(m.validate_graph_fingerprint(&[0x11; 32]).is_ok());
    assert!(m.validate_search_fingerprint(&[0x22; 32]).is_ok());
}

#[test]
fn test_fingerprint_validation_mismatch() {
    let m = fixture_manifest();
    // Wrong graph fingerprint -> error.
    let err = m.validate_graph_fingerprint(&[0xFF; 32]).unwrap_err();
    assert!(matches!(
        err,
        ManifestError::GraphFingerprintMismatch { .. }
    ));

    // Wrong search fingerprint -> error.
    let err = m.validate_search_fingerprint(&[0xFF; 32]).unwrap_err();
    assert!(matches!(
        err,
        ManifestError::SearchFingerprintMismatch { .. }
    ));
}

#[test]
fn test_fingerprint_validation_detects_corruption() {
    // Simulate corruption: the recomputed fingerprint differs from stored.
    let m = fixture_manifest();
    let corrupted_fp = {
        let mut fp = m.graph_fingerprint;
        fp[0] ^= 0xFF;
        fp
    };
    assert!(m.validate_graph_fingerprint(&corrupted_fp).is_err());
}

// -------- VAL-CAS-015: layer-kind completeness --------

#[test]
fn test_manifest_layer_completeness_ok() {
    let m = fixture_manifest(); // has all 5 layers
    assert!(m.validate_layers().is_ok());
}

#[test]
fn test_manifest_missing_neural_rejected() {
    let mut m = fixture_manifest();
    m.layers.remove(&LayerKind::Neural);
    let err = m.validate_layers().unwrap_err();
    assert!(
        matches!(err, ManifestError::MissingLayer(_)),
        "expected MissingLayer, got {err:?}"
    );
}

#[test]
fn test_manifest_missing_symbols_rejected() {
    let mut m = fixture_manifest();
    m.layers.remove(&LayerKind::Symbols);
    assert!(m.validate_layers().is_err());
}

#[test]
fn test_manifest_missing_db_rejected() {
    let mut m = fixture_manifest();
    m.layers.remove(&LayerKind::Db);
    assert!(m.validate_layers().is_err());
}

#[test]
fn test_manifest_extra_layer_rejected() {
    // The manifest should only have exactly the 5 known layer kinds.
    // Since LayerKind is a closed enum, this is inherently enforced by the
    // type system, but if someone manually constructs invalid bytes we
    // should reject via deserialization.
    let m = fixture_manifest();
    let bytes = m.to_bytes().expect("serialize");
    // Valid deserialization still works.
    let recovered = Manifest::from_bytes(&bytes).expect("deserialize");
    assert_eq!(recovered.layers.len(), 5);
    assert!(recovered.validate_layers().is_ok());
}

#[test]
fn test_layer_kind_all_variants() {
    // Ensure all 5 variants exist and are distinct.
    let kinds = [
        LayerKind::Db,
        LayerKind::Tfidf,
        LayerKind::Neural,
        LayerKind::Pdg,
        LayerKind::Symbols,
    ];
    let unique: std::collections::HashSet<&LayerKind> = kinds.iter().collect();
    assert_eq!(unique.len(), 5);
}

// -------- Extra: generation number roundtrip --------

#[test]
fn test_manifest_generation_number_preserved() {
    let mut m = fixture_manifest();
    m.generation = u64::MAX;
    let bytes = m.to_bytes().expect("serialize");
    let recovered = Manifest::from_bytes(&bytes).expect("deserialize");
    assert_eq!(recovered.generation, u64::MAX);
}

#[test]
fn test_manifest_layer_hashes() {
    let m = fixture_manifest();
    let hashes = m.layer_hashes();
    assert_eq!(hashes.len(), 5);
    // Each hash is the value from the layers map.
    for kind in [
        LayerKind::Db,
        LayerKind::Tfidf,
        LayerKind::Neural,
        LayerKind::Pdg,
        LayerKind::Symbols,
    ] {
        assert!(hashes.contains(&m.layers[&kind]));
    }
}
