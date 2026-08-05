//! Tests for CAS blob format: encode, validate, corruption rejection.

use super::*;

#[test]
fn test_blob_hash_deterministic() {
    let payload = b"hello world";
    let h1 = blob_hash(payload);
    let h2 = blob_hash(payload);
    assert_eq!(h1, h2, "same payload must produce same hash");

    // Different payload produces different hash.
    let h3 = blob_hash(b"hello world!");
    assert_ne!(h1, h3);
}

#[test]
fn test_encode_blob_structure() {
    let payload = b"test payload";
    let blob = encode_blob(payload);

    // magic
    assert_eq!(&blob[0..9], BLOB_MAGIC);
    // version
    assert_eq!(blob[9], BLOB_VERSION);
    // hash matches blob_hash
    let mut stored_hash = [0u8; 32];
    stored_hash.copy_from_slice(&blob[10..42]);
    assert_eq!(stored_hash, blob_hash(payload));
    // length
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&blob[42..50]);
    assert_eq!(u64::from_le_bytes(len_bytes), payload.len() as u64);
    // payload
    assert_eq!(&blob[50..], payload);
}

#[test]
fn test_validate_blob_ok() {
    let payload = b"a valid payload with some content";
    let blob = encode_blob(payload);
    let hash = validate_blob(&blob).expect("valid blob should validate");
    assert_eq!(hash, blob_hash(payload));
}

#[test]
fn test_validate_blob_truncated() {
    // Less than header length.
    let short = vec![0u8; 10];
    assert_eq!(
        validate_blob(&short),
        Err(BadBlob::Truncated {
            min: BLOB_HEADER_LEN,
            actual: 10,
        })
    );
}

#[test]
fn test_validate_blob_bad_magic() {
    let mut blob = encode_blob(b"some data");
    // Flip first byte of magic.
    blob[0] = b'X';
    let err = validate_blob(&blob).unwrap_err();
    assert!(matches!(err, BadBlob::BadMagic(_)));
}

#[test]
fn test_validate_blob_wrong_version() {
    let mut blob = encode_blob(b"some data");
    blob[9] = 99;
    let err = validate_blob(&blob).unwrap_err();
    assert_eq!(
        err,
        BadBlob::VersionMismatch {
            got: 99,
            expected: BLOB_VERSION,
        }
    );
}

#[test]
fn test_validate_blob_payload_truncated() {
    let mut blob = encode_blob(b"a longer payload for truncation testing!!!");
    // Remove last 5 bytes so actual payload < declared length.
    blob.truncate(blob.len() - 5);
    let err = validate_blob(&blob).unwrap_err();
    assert!(matches!(err, BadBlob::PayloadLengthMismatch { .. }));
}

#[test]
fn test_validate_blob_hash_mismatch() {
    let payload = b"original content";
    let mut blob = encode_blob(payload);
    // Flip a byte in the payload (after header) so the stored hash no longer
    // matches the recomputed hash.
    let last = blob.len() - 1;
    blob[last] ^= 0xFF;
    let err = validate_blob(&blob).unwrap_err();
    assert_eq!(err, BadBlob::HashMismatch);
}

#[test]
fn test_extract_payload_roundtrip() {
    let payload = b"roundtrip payload bytes";
    let blob = encode_blob(payload);
    let (extracted, hash) = extract_payload(&blob).expect("valid blob");
    assert_eq!(extracted, payload);
    assert_eq!(hash, blob_hash(payload));
}

#[test]
fn test_blob_hash_empty() {
    let h = blob_hash(b"");
    // blake3 of empty input — known reference value.
    let expected: [u8; 32] = blake3::hash(b"").into();
    assert_eq!(h, expected);
}

#[test]
fn test_blob_hash_large_binary() {
    let payload: Vec<u8> = (0..8192u32).map(|i| (i & 0xFF) as u8).collect();
    let h = blob_hash(&payload);
    let expected: [u8; 32] = blake3::hash(&payload).into();
    assert_eq!(h, expected);

    let blob = encode_blob(&payload);
    assert_eq!(validate_blob(&blob).unwrap(), h);
}

#[test]
fn test_hex_roundtrip() {
    let hash = blob_hash(b"hex roundtrip test");
    let hex = hash_to_hex(&hash);
    assert_eq!(hex.len(), 64);
    let recovered = hex_to_hash(&hex).expect("hex should parse");
    assert_eq!(recovered, hash);
}

#[test]
fn test_hex_invalid_length() {
    assert!(hex_to_hash("abc").is_none());
    assert!(hex_to_hash(&"x".repeat(64)).is_none());
}

#[test]
fn test_bad_magic_alternate_values() {
    // Invalid magic: wrong version tag.
    for magic in [&b"LIDX-BLB0"[..], &b"LIDX-GEN1"[..], &b"GEN00000!"[..9]] {
        let mut blob = encode_blob(b"data");
        blob[0..9].copy_from_slice(magic);
        assert!(matches!(
            validate_blob(&blob).unwrap_err(),
            BadBlob::BadMagic(_)
        ));
    }
}
