//! CAS blob format: magic + version + hash + length + payload.
//!
//! Each blob on disk is a self-describing frame that embeds its own blake3
//! content hash, so integrity can be verified independently of the file path.

use std::io;

/// 9-byte magic header identifying a LeIndex CAS blob.
pub const BLOB_MAGIC: &[u8; 9] = b"LIDX-BLB1";

/// Current blob format version.
pub const BLOB_VERSION: u8 = 1;

/// Fixed header size: magic (9) + version (1) + hash (32) + length (8) = 50 bytes.
pub const BLOB_HEADER_LEN: usize = 9 + 1 + 32 + 8;

/// Compute the blake3 hash of `payload`, returning the raw 32-byte digest.
pub fn blob_hash(payload: &[u8]) -> [u8; 32] {
    blake3::hash(payload).into()
}

/// Encode `payload` into a self-describing blob frame.
///
/// Layout: `[BLOB_MAGIC(9)] [version(1)] [hash(32)] [payload_len_u64_le(8)] [payload..]`.
pub fn encode_blob(payload: &[u8]) -> Vec<u8> {
    let hash = blob_hash(payload);
    let payload_len = payload.len() as u64;
    let mut buf = Vec::with_capacity(BLOB_HEADER_LEN + payload.len());
    buf.extend_from_slice(BLOB_MAGIC);
    buf.push(BLOB_VERSION);
    buf.extend_from_slice(&hash);
    buf.extend_from_slice(&payload_len.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Errors that arise when validating a blob frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BadBlob {
    /// The blob is shorter than the fixed header.
    #[error("blob truncated: {actual} bytes, need at least {min} for header")]
    Truncated {
        /// Required minimum size (header length).
        min: usize,
        /// Actual bytes present.
        actual: usize,
    },
    /// The magic bytes do not match `LIDX-BLB1`.
    #[error("bad magic header: got {0:?}")]
    BadMagic([u8; 9]),
    /// The version byte is not supported by this reader.
    #[error("unsupported version: got {got}, expected {expected}")]
    VersionMismatch {
        /// Version byte found on disk.
        got: u8,
        /// Version this reader expects.
        expected: u8,
    },
    /// The declared payload length disagrees with the actual frame size.
    #[error(
        "payload length mismatch: header says {header_len}, frame has {frame_len} bytes of payload"
    )]
    PayloadLengthMismatch {
        /// Declared payload length from the header.
        header_len: u64,
        /// Actual bytes remaining after the header.
        frame_len: u64,
    },
    /// The recomputed blake3 of the payload does not match the stored hash.
    #[error("hash mismatch: stored hash does not match recomputed payload hash")]
    HashMismatch,
}

/// Validate a blob frame and return the stored blake3 hash.
///
/// Re-hashes the payload and compares against the hash embedded in the header.
pub fn validate_blob(bytes: &[u8]) -> Result<[u8; 32], BadBlob> {
    if bytes.len() < BLOB_HEADER_LEN {
        return Err(BadBlob::Truncated {
            min: BLOB_HEADER_LEN,
            actual: bytes.len(),
        });
    }
    let mut magic = [0u8; 9];
    magic.copy_from_slice(&bytes[0..9]);
    if &magic != BLOB_MAGIC {
        return Err(BadBlob::BadMagic(magic));
    }
    let version = bytes[9];
    if version != BLOB_VERSION {
        return Err(BadBlob::VersionMismatch {
            got: version,
            expected: BLOB_VERSION,
        });
    }
    let mut stored_hash = [0u8; 32];
    stored_hash.copy_from_slice(&bytes[10..42]);
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&bytes[42..50]);
    let payload_len = u64::from_le_bytes(len_bytes);

    let remaining = bytes.len() as u64 - BLOB_HEADER_LEN as u64;
    if remaining != payload_len {
        return Err(BadBlob::PayloadLengthMismatch {
            header_len: payload_len,
            frame_len: remaining,
        });
    }
    let payload = &bytes[BLOB_HEADER_LEN..BLOB_HEADER_LEN + payload_len as usize];
    let computed = blob_hash(payload);
    if computed != stored_hash {
        return Err(BadBlob::HashMismatch);
    }
    Ok(stored_hash)
}

/// Validate the blob, then return a reference to the payload slice and the hash.
pub fn extract_payload(bytes: &[u8]) -> Result<(&[u8], [u8; 32]), BadBlob> {
    let hash = validate_blob(bytes)?;
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&bytes[42..50]);
    let payload_len = u64::from_le_bytes(len_bytes) as usize;
    Ok((&bytes[BLOB_HEADER_LEN..BLOB_HEADER_LEN + payload_len], hash))
}

/// Convert a 32-byte hash to a lowercase hex string.
pub fn hash_to_hex(hash: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for byte in hash {
        write!(&mut s, "{byte:02x}").expect("formatting into String never fails");
    }
    s
}

/// Parse a 64-character hex string back into a 32-byte hash.
pub fn hex_to_hash(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut hash = [0u8; 32];
    let bytes = hex.as_bytes();
    for (i, byte) in hash.iter_mut().enumerate() {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(hash)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Fsync a file handle, ignoring `EINVAL` (which some filesystems return for
/// intermediate buffering layers where fsync is a no-op).
pub(crate) fn fsync_file(file: &std::fs::File) -> io::Result<()> {
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "blob_test.rs"]
mod tests;
