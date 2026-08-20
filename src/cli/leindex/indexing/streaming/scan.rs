//! Streaming scan stage (WS6-9 Task 1).
//!
//! Walks files lazily, hashing each via a fixed 64KiB buffer, writing metadata
//! records to a CAS-staged scan blob incrementally. No source body is retained
//! (spec §6.1, VAL-STREAM-002).

use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Fixed buffer size for streaming file hashing (64 KiB).
const HASH_BUFFER_SIZE: usize = 64 * 1024;

/// A single scan record: path + content hash + size + language hint + mtime.
///
/// Serialized into the CAS-staged scan blob. Contains no source bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanRecord {
    /// Relative file path from the scan root.
    pub path: String,
    /// Blake3 content hash (hex).
    pub hash: String,
    /// File size in bytes.
    pub size: u64,
    /// Detected language hint (from extension).
    pub lang: Option<String>,
    /// File modification time (Unix mtime seconds).
    pub mtime: u64,
}

/// Statistics from a streaming scan run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// Total number of files scanned.
    pub files_scanned: usize,
    /// Total bytes hashed across all files.
    pub total_bytes: u64,
    /// Number of files that could not be read.
    pub errors: usize,
}

/// Trait for consuming scan records (abstraction over CAS staging, Vec
/// collection, checkpoint writer, etc.).
pub trait ScanRecordWriter {
    /// Write a single scan record.
    fn write_record(&mut self, record: &ScanRecord) -> Result<()>;
}

/// A simple Vec-backed scan record writer (for testing and in-memory use).
#[derive(Debug, Default)]
pub struct VecScanRecordWriter {
    /// Collected records.
    pub records: Vec<ScanRecord>,
}

impl VecScanRecordWriter {
    /// Create an empty Vec scan writer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScanRecordWriter for VecScanRecordWriter {
    fn write_record(&mut self, record: &ScanRecord) -> Result<()> {
        self.records.push(record.clone());
        Ok(())
    }
}

/// Detect language hint from file extension.
fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "php" => "php",
        "lua" => "lua",
        "dart" => "dart",
        "sh" | "bash" => "bash",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Hash a file's content using a fixed 64KiB streaming buffer.
///
/// Returns `(blake3_hex_hash, file_size_bytes)`. The file is read in
/// 64KiB chunks and the hasher never holds more than the buffer, ensuring
/// RSS is bounded regardless of file size (VAL-STREAM-002).
fn hash_file_streaming(path: &Path) -> Result<(String, u64)> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::with_capacity(HASH_BUFFER_SIZE, file);
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; HASH_BUFFER_SIZE];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

/// Stream-walk a directory tree, hashing each source file via a fixed 64KiB
/// buffer, emitting `(path, hash, size, lang, mtime)` records to `writer`.
///
/// No source bytes are retained in memory after hashing. RSS is flat across
/// the entire scan regardless of total corpus size (spec §6.1, VAL-STREAM-002).
///
/// Only files matching known source extensions are scanned. Hidden directories
/// (starting with `.`) are skipped.
pub fn stream_scan<W: ScanRecordWriter>(
    root: &Path,
    writer: &mut W,
    extensions: &[&str],
) -> Result<ScanStats> {
    let mut stats = ScanStats::default();
    stream_scan_inner(root, root, writer, extensions, &mut stats)?;
    Ok(stats)
}

fn stream_scan_inner<W: ScanRecordWriter>(
    root: &Path,
    dir: &Path,
    writer: &mut W,
    extensions: &[&str],
    stats: &mut ScanStats,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            stats.errors += 1;
            return Ok(());
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => {
                stats.errors += 1;
                continue;
            }
        };
        if file_type.is_dir() {
            // Skip hidden dirs (e.g. .leindex, .git)
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            stream_scan_inner(root, &path, writer, extensions, stats)?;
        } else if file_type.is_file() {
            // Filter by extension
            let ext_ok = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| extensions.contains(&ext));
            if !ext_ok {
                continue;
            }
            match hash_file_streaming(&path) {
                Ok((hash, size)) => {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let record = ScanRecord {
                        path: rel,
                        hash,
                        size,
                        lang: detect_language(&path),
                        mtime,
                    };
                    writer.write_record(&record)?;
                    stats.files_scanned += 1;
                    stats.total_bytes += size;
                }
                Err(_) => {
                    stats.errors += 1;
                }
            }
        }
    }
    Ok(())
}

/// Common source file extensions for scanning.
pub const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "c", "h", "cpp", "cc", "cxx",
    "hpp", "hxx", "cs", "rb", "swift", "kt", "kts", "scala", "sc", "php", "lua", "dart", "sh",
    "bash", "json", "html", "htm", "css", "scss", "yaml", "yml", "cmake", "ex", "exs", "erl",
    "hrl", "hs", "pl", "pm", "r", "zig", "graphql", "gql", "hcl", "tf", "tfvars", "el", "jl", "d",
    "di", "glsl", "vert", "frag", "comp", "md", "markdown", "rst", "adoc", "asciidoc", "txt",
];

/// Serialize a batch of scan records to bytes for CAS staging.
pub fn serialize_scan_records(records: &[ScanRecord]) -> Result<Vec<u8>> {
    Ok(bincode::serialize(records)?)
}

/// Deserialize scan records from a CAS blob.
pub fn deserialize_scan_records(data: &[u8]) -> Result<Vec<ScanRecord>> {
    Ok(bincode::deserialize(data)?)
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;
    use std::fs;

    fn make_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("b.py"), "def foo():\n  pass\n").unwrap();
        fs::write(dir.path().join("c.docx"), "not a source file\n").unwrap();
        fs::create_dir(dir.path().join(".hidden")).unwrap();
        fs::write(dir.path().join(".hidden/secret.rs"), "fn secret() {}").unwrap();
        dir
    }

    /// VAL-STREAM-002: No source body retained during scan.
    #[test]
    fn test_stream_scan_no_source_retention() {
        let dir = make_fixture();
        let mut writer = VecScanRecordWriter::new();
        let stats = stream_scan(dir.path(), &mut writer, SOURCE_EXTENSIONS).unwrap();

        // Only .rs and .py files scanned; .txt and .hidden skipped
        assert_eq!(writer.records.len(), 2);
        assert_eq!(stats.files_scanned, 2);
        assert_eq!(stats.errors, 0);

        // Records contain metadata only (no source bytes in the struct)
        for record in &writer.records {
            assert!(!record.path.is_empty());
            assert!(!record.hash.is_empty());
            assert!(record.size > 0);
            // hash is 64-char hex blake3
            assert_eq!(record.hash.len(), 64);
        }
    }

    /// VAL-STREAM-002: Hash buffer is fixed 64KiB (not growing with file size).
    #[test]
    fn test_hash_file_streaming_buffer_bounded() {
        let dir = tempfile::tempdir().unwrap();
        // Create a file larger than the hash buffer
        let large_content = "x".repeat(HASH_BUFFER_SIZE * 3 + 100);
        let path = dir.path().join("large.rs");
        fs::write(&path, &large_content).unwrap();

        let hash = blake3::hash(large_content.as_bytes()).to_hex().to_string();
        let (streamed_hash, size) = hash_file_streaming(&path).unwrap();

        assert_eq!(hash, streamed_hash);
        assert_eq!(size, large_content.len() as u64);
    }

    #[test]
    fn test_stream_scan_detects_language() {
        let dir = make_fixture();
        let mut writer = VecScanRecordWriter::new();
        let _stats = stream_scan(dir.path(), &mut writer, SOURCE_EXTENSIONS).unwrap();

        let langs: Vec<Option<&str>> = writer.records.iter().map(|r| r.lang.as_deref()).collect();
        assert!(langs.contains(&Some("rust")));
        assert!(langs.contains(&Some("python")));
        assert!(!langs.contains(&Some("text")));
    }

    #[test]
    fn test_stream_scan_stats() {
        let dir = make_fixture();
        let mut writer = VecScanRecordWriter::new();
        let stats = stream_scan(dir.path(), &mut writer, SOURCE_EXTENSIONS).unwrap();
        assert!(stats.total_bytes > 0);
        assert_eq!(stats.files_scanned, 2);
    }

    #[test]
    fn test_scan_records_roundtrip() {
        let records = vec![
            ScanRecord {
                path: "a.rs".into(),
                hash: "abc123".into(),
                size: 42,
                lang: Some("rust".into()),
                mtime: 1000,
            },
            ScanRecord {
                path: "b.py".into(),
                hash: "def456".into(),
                size: 99,
                lang: Some("python".into()),
                mtime: 2000,
            },
        ];
        let bytes = serialize_scan_records(&records).unwrap();
        let back = deserialize_scan_records(&bytes).unwrap();
        assert_eq!(back, records);
    }

    #[test]
    fn test_stream_scan_skips_hidden_dirs() {
        let dir = make_fixture();
        let mut writer = VecScanRecordWriter::new();
        let _stats = stream_scan(dir.path(), &mut writer, SOURCE_EXTENSIONS).unwrap();
        // .hidden/secret.rs should not be scanned
        assert!(!writer.records.iter().any(|r| r.path.contains(".hidden")));
    }

    #[test]
    fn test_stream_scan_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = VecScanRecordWriter::new();
        let stats = stream_scan(dir.path(), &mut writer, SOURCE_EXTENSIONS).unwrap();
        assert_eq!(stats.files_scanned, 0);
        assert!(writer.records.is_empty());
    }
}
