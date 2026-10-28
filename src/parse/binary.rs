//! Bounded metadata extraction for PE/DLL and selected game assets.
//!
//! Binary inputs deliberately bypass tree-sitter. The extractor reads at most
//! `MAX_BINARY_BYTES`, keeps only printable strings up to `MAX_STRINGS`, and
//! returns deterministic metadata suitable for search/index records.

use std::path::Path;

/// Maximum bytes read from a binary input.
pub const MAX_BINARY_BYTES: usize = 8 * 1024 * 1024;
/// Maximum printable strings retained from one binary input.
pub const MAX_STRINGS: usize = 256;
/// Maximum length of one retained printable string.
pub const MAX_STRING_LEN: usize = 256;

/// Explicitly supported opaque game/container extensions.
pub const GAME_FILE_EXTENSIONS: &[&str] = &[
    "pak", "wad", "uasset", "umap", "unity3d", "assets", "bundle", "res", "scn", "rpak",
];

/// Extensions treated as PE/DLL binary inputs.
pub const BINARY_FILE_EXTENSIONS: &[&str] = &["dll", "exe", "ocx", "sys"];

/// Classification of an input before parser dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryKind {
    /// Portable Executable (including DLLs).
    Pe,
    /// Known game/container asset.
    GameAsset,
}

/// Extracted, deterministic binary record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryRecord {
    /// Stable language/type label for index records.
    pub language: &'static str,
    /// Searchable metadata text.
    pub text: String,
}

/// Classify by extension and, for unknown extensions, by the PE magic.
pub fn classify(path: &Path, bytes: &[u8]) -> Option<BinaryKind> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    if ext
        .as_deref()
        .is_some_and(|e| BINARY_FILE_EXTENSIONS.contains(&e))
    {
        return Some(BinaryKind::Pe);
    }
    if ext
        .as_deref()
        .is_some_and(|e| GAME_FILE_EXTENSIONS.contains(&e))
    {
        return Some(BinaryKind::GameAsset);
    }
    (bytes.len() >= 2 && bytes[..2] == *b"MZ").then_some(BinaryKind::Pe)
}

/// Extract bounded metadata and strings. Malformed PE data is still useful as
/// an opaque binary: the record contains its kind and strings, never panics.
pub fn extract(path: &Path, bytes: &[u8]) -> BinaryRecord {
    let kind = classify(path, bytes).unwrap_or(BinaryKind::GameAsset);
    let bounded = &bytes[..bytes.len().min(MAX_BINARY_BYTES)];
    let mut lines = vec![format!(
        "binary_kind={}",
        match kind {
            BinaryKind::Pe => "pe",
            BinaryKind::GameAsset => "game_asset",
        }
    )];
    lines.push(format!("size_bytes={}", bytes.len()));
    if kind == BinaryKind::Pe {
        lines.extend(pe_metadata(bounded));
    }
    lines.extend(
        printable_strings(bounded)
            .into_iter()
            .map(|s| format!("string={s}")),
    );
    BinaryRecord {
        language: "binary",
        text: lines.join("\n"),
    }
}

fn pe_metadata(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        out.push("pe_valid=false".into());
        return out;
    }
    let pe_offset = u32::from_le_bytes(bytes[0x3c..0x40].try_into().unwrap()) as usize;
    if pe_offset
        .checked_add(24)
        .is_none_or(|end| end > bytes.len())
        || &bytes[pe_offset..pe_offset + 4] != b"PE\0\0"
    {
        out.push("pe_valid=false".into());
        return out;
    }
    let coff = pe_offset + 4;
    let machine = u16::from_le_bytes(bytes[coff..coff + 2].try_into().unwrap());
    let sections = u16::from_le_bytes(bytes[coff + 2..coff + 4].try_into().unwrap());
    let characteristics = u16::from_le_bytes(bytes[coff + 18..coff + 20].try_into().unwrap());
    out.push("pe_valid=true".into());
    out.push(format!("machine=0x{machine:04x}"));
    out.push(format!("sections={sections}"));
    out.push(format!("characteristics=0x{characteristics:04x}"));
    if coff + 22 <= bytes.len() {
        let magic = u16::from_le_bytes(bytes[coff + 20..coff + 22].try_into().unwrap());
        out.push(format!("optional_header_magic=0x{magic:04x}"));
    }
    out
}

fn printable_strings(bytes: &[u8]) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for &byte in bytes {
        if byte.is_ascii_graphic() || byte == b' ' {
            if current.len() < MAX_STRING_LEN {
                current.push(byte as char);
            }
        } else {
            if current.len() >= 4 {
                result.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            if result.len() >= MAX_STRINGS {
                break;
            }
        }
    }
    if current.len() >= 4 && result.len() < MAX_STRINGS {
        result.push(current);
    }
    result.sort();
    result.dedup();
    result
}

#[cfg(test)]
mod test {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_classifies_pe_game_and_source() {
        assert_eq!(classify(Path::new("x.dll"), b"MZ"), Some(BinaryKind::Pe));
        assert_eq!(
            classify(Path::new("x.uasset"), b"garbage"),
            Some(BinaryKind::GameAsset)
        );
        assert_eq!(classify(Path::new("x.cpp"), b"int x;"), None);
    }

    #[test]
    fn test_malformed_pe_is_safe_and_deterministic() {
        let record = extract(Path::new("bad.dll"), b"MZhello_asset");
        assert!(record.text.contains("pe_valid=false"));
        assert_eq!(record, extract(Path::new("bad.dll"), b"MZhello_asset"));
    }

    #[test]
    fn test_extraction_is_bounded() {
        let bytes = vec![b'a'; MAX_BINARY_BYTES + 1024];
        let record = extract(Path::new("x.pak"), &bytes);
        assert!(record.text.len() < MAX_STRINGS * (MAX_STRING_LEN + 8));
    }
}
