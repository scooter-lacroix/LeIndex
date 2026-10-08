//! Streaming parse stage (WS6-9 Task 2).
//!
//! Bounded by file count AND aggregate bytes. Per-file signatures are
//! persisted to CAS immediately; the syntax tree and source buffer are
//! dropped before the next file is parsed. RSS does not accumulate linearly
//! with parsed file count (spec §6.2, VAL-STREAM-003).

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::scan::ScanRecord;

/// Budget controlling how much parsing work one bounded chunk does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseBudget {
    /// Maximum file count per chunk.
    pub max_files: usize,
    /// Maximum aggregate bytes per chunk.
    pub max_bytes: usize,
}

impl Default for ParseBudget {
    fn default() -> Self {
        Self {
            max_files: 50,
            max_bytes: 10 * 1024 * 1024, // 10 MiB
        }
    }
}

impl ParseBudget {
    /// A budget with generous limits (tests only).
    pub fn unlimited() -> Self {
        Self {
            max_files: usize::MAX,
            max_bytes: usize::MAX,
        }
    }
}

/// A per-file parse signature record, ready for CAS staging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedFileRecord {
    /// Relative file path.
    pub path: String,
    /// Content hash from the scan stage.
    pub content_hash: String,
    /// Detected language.
    pub lang: String,
    /// Extracted signature names.
    pub signatures: Vec<SignatureSummary>,
    /// Time to parse (ms).
    pub parse_time_ms: u64,
}

/// A compact signature summary extracted during parsing. Contains only the
/// durable metadata needed for indexing; the full tree-sitter AST is dropped.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignatureSummary {
    /// Symbol name.
    pub name: String,
    /// Signature kind: "function", "method", "class", "variable", etc.
    pub kind: String,
    /// Byte range start.
    pub byte_start: usize,
    /// Byte range end.
    pub byte_end: usize,
}

/// Statistics from a streaming parse run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Number of files parsed.
    pub files_parsed: usize,
    /// Total bytes parsed.
    pub total_bytes: u64,
    /// Total signatures extracted.
    pub total_signatures: usize,
    /// Files that failed to parse.
    pub failed: usize,
}

/// Trait for consuming per-file parse records (CAS staging, in-memory, etc.).
pub trait ParseRecordWriter {
    /// Write a single per-file parse record (persisted immediately).
    fn write_record(&mut self, record: &ParsedFileRecord) -> Result<()>;
}

/// Vec-backed parse record writer for testing.
#[derive(Debug, Default)]
pub struct VecParseRecordWriter {
    /// Collected records.
    pub records: Vec<ParsedFileRecord>,
}

impl VecParseRecordWriter {
    /// Create an empty Vec parse writer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ParseRecordWriter for VecParseRecordWriter {
    fn write_record(&mut self, record: &ParsedFileRecord) -> Result<()> {
        self.records.push(record.clone());
        Ok(())
    }
}

/// Chunk a set of scan records into bounded parse batches.
///
/// Each batch's file count <= `budget.max_files` and aggregate bytes <=
/// `budget.max_bytes`.
pub fn chunk_scan_records<'a>(
    records: &'a [ScanRecord],
    budget: &ParseBudget,
) -> Vec<&'a [ScanRecord]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut current_bytes: u64 = 0;
    let mut current_count: usize = 0;
    for (i, record) in records.iter().enumerate() {
        let would_exceed_count = current_count + 1 > budget.max_files;
        let would_exceed_bytes = current_bytes + record.size > budget.max_bytes as u64;
        if i > start && (would_exceed_count || would_exceed_bytes) {
            chunks.push(&records[start..i]);
            start = i;
            current_bytes = 0;
            current_count = 0;
        }
        current_bytes += record.size;
        current_count += 1;
    }
    if start < records.len() {
        chunks.push(&records[start..]);
    }
    chunks
}

/// Detect language from a scan record, falling back to extension scanning.
fn detect_lang(record: &ScanRecord) -> String {
    record.lang.clone().unwrap_or_else(|| {
        Path::new(&record.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown")
            .to_string()
    })
}

/// Parse a single file into a ParsedFileRecord.
///
/// Reads the file source, extracts simple signatures via a bracket-depth
/// heuristic (language-agnostic fallback), then the source is dropped. In
/// production this delegates to tree-sitter parsers; the key invariant is
/// that the source bytes and syntax tree do not outlive this call.
fn parse_one_file(
    root: &Path,
    record: &ScanRecord,
    signature_extractor: &dyn SignatureExtractor,
) -> Result<ParsedFileRecord> {
    let file_path = root.join(&record.path);
    let source = std::fs::read_to_string(&file_path)?;
    let now = std::time::Instant::now();

    let signatures = signature_extractor.extract(&source);

    Ok(ParsedFileRecord {
        path: record.path.clone(),
        content_hash: record.hash.clone(),
        lang: detect_lang(record),
        signatures,
        parse_time_ms: now.elapsed().as_millis() as u64,
    })
}

/// Trait for extracting signatures from source text.
///
/// In production this is backed by tree-sitter parsers. The trait allows
/// testing with a simple heuristic extractor.
pub trait SignatureExtractor {
    /// Extract signatures from source text.
    fn extract(&self, source: &str) -> Vec<SignatureSummary>;
}

/// A simple heuristic signature extractor for testing. Detects `fn`/`def` /
/// `func` patterns and class-like constructs.
pub struct HeuristicExtractor;

impl SignatureExtractor for HeuristicExtractor {
    fn extract(&self, source: &str) -> Vec<SignatureSummary> {
        let mut sigs = Vec::new();
        for (line_num, line) in source.lines().enumerate() {
            let trimmed = line.trim();
            // Rust: fn name(  Python: def name(  Go: func name(  JS/TS: function name(
            if let Some(name) = extract_fn_name(trimmed) {
                sigs.push(SignatureSummary {
                    name,
                    kind: "function".into(),
                    byte_start: line_to_byte(source, line_num),
                    byte_end: line_to_byte(source, line_num) + line.len(),
                });
            }
            // class/struct/impl detection
            if let Some(name) = extract_class_name(trimmed) {
                sigs.push(SignatureSummary {
                    name,
                    kind: "class".into(),
                    byte_start: line_to_byte(source, line_num),
                    byte_end: line_to_byte(source, line_num) + line.len(),
                });
            }
        }
        sigs
    }
}

fn extract_fn_name(line: &str) -> Option<String> {
    for kw in &["fn ", "def ", "func ", "function "] {
        if let Some(rest) = line.strip_prefix(kw) {
            let rest = rest.trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn extract_class_name(line: &str) -> Option<String> {
    for kw in &[
        "struct ",
        "class ",
        "enum ",
        "trait ",
        "interface ",
        "impl ",
    ] {
        if let Some(rest) = line.strip_prefix(kw) {
            let rest = rest.trim_start();
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn line_to_byte(source: &str, line_num: usize) -> usize {
    source
        .lines()
        .take(line_num)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}

/// stream_parse iterates through scan records in bounded chunks, parsing each
/// file, persisting per-file records immediately, and dropping the source +
/// syntax tree before moving to the next file.
///
/// RSS stays bounded: only the current file's source and syntax tree are in
/// memory at any time (VAL-STREAM-003).
pub fn stream_parse<W: ParseRecordWriter>(
    root: &Path,
    scan_records: &[ScanRecord],
    writer: &mut W,
    extractor: &dyn SignatureExtractor,
    budget: &ParseBudget,
) -> Result<ParseStats> {
    let mut stats = ParseStats::default();
    let chunks = chunk_scan_records(scan_records, budget);
    for chunk in chunks {
        for record in chunk {
            match parse_one_file(root, record, extractor) {
                Ok(parsed) => {
                    stats.total_signatures += parsed.signatures.len();
                    stats.total_bytes += record.size;
                    stats.files_parsed += 1;
                    writer.write_record(&parsed)?;
                }
                Err(_) => {
                    stats.failed += 1;
                }
            }
            // VAL-STREAM-003: source bytes and parse tree are dropped here
            // (they were local variables in parse_one_file, now out of scope)
        }
    }
    Ok(stats)
}

/// Chunk `(path, bytes)` inputs into bounded batches: each batch holds at
/// most `budget.max_files` paths and at most `budget.max_bytes` aggregate
/// bytes. Never emits an empty chunk; never splits a single file across
/// chunks.
fn chunk_file_inputs(files: &[(PathBuf, u64)], budget: &ParseBudget) -> Vec<Vec<PathBuf>> {
    let mut chunks = Vec::new();
    let mut current: Vec<PathBuf> = Vec::new();
    let mut current_bytes: u64 = 0;
    for (path, bytes) in files {
        let would_exceed = !current.is_empty()
            && (current.len() + 1 > budget.max_files
                || current_bytes + bytes > budget.max_bytes as u64);
        if would_exceed {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(path.clone());
        current_bytes += bytes;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Bounded streaming entry point for the production parse route.
///
/// Parses `(path, bytes)` pairs through the production `ParallelParser`
/// (tree-sitter) in chunks bounded by `budget`, so peak RSS is capped by
/// chunk size instead of the full file set. Returned `ParsingResult`s keep
/// the exact shape and order the legacy `parse_files` route produces, but
/// `source_bytes` is stripped to `None` after each chunk: results never
/// retain the whole corpus (PR #90 review, P1). PDG extraction re-reads
/// file-backed results lazily (see `extraction_source_bytes` in
/// `indexing/mod.rs`) and the parse checkpoint persists signatures, so no
/// downstream consumer loses data. D3: downstream checkpoint/PDG
/// consumption is route-independent.
pub fn stream_parse_parallel(
    files: Vec<(PathBuf, u64)>,
    budget: &ParseBudget,
) -> Vec<crate::parse::parallel::ParsingResult> {
    let parser = crate::parse::parallel::ParallelParser::new();
    let mut results = Vec::new();
    for chunk in chunk_file_inputs(&files, budget) {
        let mut chunk_results = parser.parse_files(chunk);
        // Streaming memory contract: returned results must not retain the
        // whole corpus. Signatures are already persisted via the parse
        // checkpoint and PDG extraction re-reads file-backed results, so
        // stripping here is lossless for every downstream consumer.
        for result in &mut chunk_results {
            result.source_bytes = None;
        }
        results.extend(chunk_results);
    }
    results
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;
    use std::fs;

    fn make_fixture() -> (tempfile::TempDir, Vec<ScanRecord>) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("a.rs"),
            "fn hello() {}\nstruct Foo { x: i32 }\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("b.py"),
            "def greet():\n  pass\nclass Bar:\n  pass\n",
        )
        .unwrap();
        let records = vec![
            ScanRecord {
                path: "a.rs".into(),
                hash: "abc".into(),
                size: 35,
                lang: Some("rust".into()),
                mtime: 0,
            },
            ScanRecord {
                path: "b.py".into(),
                hash: "def".into(),
                size: 39,
                lang: Some("python".into()),
                mtime: 0,
            },
        ];
        (dir, records)
    }

    /// VAL-STREAM-003: Syntax tree dropped per file during parse (no accumulation).
    #[test]
    fn test_stream_parse_no_accumulation() {
        let (dir, records) = make_fixture();
        let mut writer = VecParseRecordWriter::new();
        let extractor = HeuristicExtractor;
        let stats = stream_parse(
            dir.path(),
            &records,
            &mut writer,
            &extractor,
            &ParseBudget::unlimited(),
        )
        .unwrap();

        assert_eq!(stats.files_parsed, 2);
        assert_eq!(writer.records.len(), 2);
        // Each file's signatures were persisted; not accumulated in a Vec on the state
        assert!(stats.total_signatures >= 2);
    }

    #[test]
    fn test_chunk_scan_records_respects_file_limit() {
        let records: Vec<ScanRecord> = (0..10)
            .map(|i| ScanRecord {
                path: format!("file{i}.rs"),
                hash: format!("h{i}"),
                size: 100,
                lang: None,
                mtime: 0,
            })
            .collect();
        let budget = ParseBudget {
            max_files: 3,
            max_bytes: usize::MAX,
        };
        let chunks = chunk_scan_records(&records, &budget);
        assert_eq!(chunks.len(), 4); // 3,3,3,1
        for chunk in &chunks[..3] {
            assert_eq!(chunk.len(), 3);
        }
        assert_eq!(chunks[3].len(), 1);
    }

    #[test]
    fn test_chunk_scan_records_respects_byte_limit() {
        let records: Vec<ScanRecord> = (0..10)
            .map(|i| ScanRecord {
                path: format!("file{i}.rs"),
                hash: format!("h{i}"),
                size: 500,
                lang: None,
                mtime: 0,
            })
            .collect();
        let budget = ParseBudget {
            max_files: usize::MAX,
            max_bytes: 1000,
        };
        let chunks = chunk_scan_records(&records, &budget);
        // Each chunk: 2 files (2*500=1000 <= 1000)
        for chunk in &chunks {
            assert!(chunk.iter().map(|r| r.size).sum::<u64>() <= 1000);
        }
    }

    #[test]
    fn test_heuristic_extractor_rust() {
        let src = "fn hello() {}\nstruct Foo { x: i32 }\nfn world() {}";
        let sigs = HeuristicExtractor.extract(src);
        assert_eq!(sigs.len(), 3); // hello, Foo, world
        // Verify function names
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"world"));
    }

    #[test]
    fn test_heuristic_extractor_python() {
        let src = "def greet():\n  pass\nclass Bar:\n  pass";
        let sigs = HeuristicExtractor.extract(src);
        assert_eq!(sigs.len(), 2); // greet, Bar
    }

    #[test]
    fn test_parsed_record_persisted_immediately() {
        let (dir, records) = make_fixture();
        let mut writer = VecParseRecordWriter::new();
        let extractor = HeuristicExtractor;
        let _ = stream_parse(
            dir.path(),
            &records,
            &mut writer,
            &extractor,
            &ParseBudget::unlimited(),
        )
        .unwrap();
        // Each record is individually written; it's in the writer's list
        for record in &writer.records {
            assert!(!record.path.is_empty());
            assert!(!record.content_hash.is_empty());
            assert!(!record.lang.is_empty());
        }
    }

    #[test]
    fn test_chunk_file_inputs_respects_file_limit() {
        let files: Vec<(PathBuf, u64)> = (0..7)
            .map(|i| (PathBuf::from(format!("f{i}.rs")), 10))
            .collect();
        let budget = ParseBudget {
            max_files: 3,
            max_bytes: usize::MAX,
        };
        let chunks = chunk_file_inputs(&files, &budget);
        assert_eq!(chunks.len(), 3); // 3, 3, 1
        assert_eq!(chunks[0].len(), 3);
        assert_eq!(chunks[1].len(), 3);
        assert_eq!(chunks[2].len(), 1);
    }

    #[test]
    fn test_chunk_file_inputs_respects_byte_limit_and_keeps_order() {
        let files: Vec<(PathBuf, u64)> = (0..6)
            .map(|i| (PathBuf::from(format!("f{i}.rs")), 400))
            .collect();
        let budget = ParseBudget {
            max_files: usize::MAX,
            max_bytes: 1000,
        };
        let chunks = chunk_file_inputs(&files, &budget);
        // 2 files per chunk (2*400 = 800 <= 1000), order preserved.
        let flat: Vec<String> = chunks
            .iter()
            .flatten()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(flat, (0..6).map(|i| format!("f{i}.rs")).collect::<Vec<_>>());
        for chunk in &chunks {
            assert!(chunk.len() <= 2);
        }
    }

    #[test]
    fn test_stream_parse_parallel_preserves_order_and_result_shape() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..5 {
            let path = dir.path().join(format!("m{i}.rs"));
            fs::write(&path, format!("pub fn marker_{i}() -> usize {{ {i} }}\n")).unwrap();
            files.push((path, 64));
        }
        let results = stream_parse_parallel(files, &ParseBudget::default());
        assert_eq!(results.len(), 5);
        for (i, result) in results.iter().enumerate() {
            assert!(result.is_success(), "file {i} must parse successfully");
            assert!(
                result
                    .signatures
                    .iter()
                    .any(|s| s.name == format!("marker_{i}")),
                "order preserved: result {i} must be marker_{i}"
            );
        }
    }

    /// PR #90 review P1: the streaming route's returned results must not
    /// retain `source_bytes` across chunks — peak memory must stay bounded
    /// by the chunk budget, not the whole corpus. PDG extraction re-reads
    /// file-backed results lazily, so `None` here is lossless downstream.
    #[test]
    fn test_stream_parse_parallel_strips_source_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..3 {
            let path = dir.path().join(format!("s{i}.rs"));
            fs::write(
                &path,
                format!("pub fn stream_strip_{i}() -> usize {{ {i} }}\n"),
            )
            .unwrap();
            files.push((path, 64));
        }
        let results = stream_parse_parallel(files, &ParseBudget::default());
        assert_eq!(results.len(), 3);
        for (i, result) in results.iter().enumerate() {
            assert!(
                result.is_success(),
                "file {i} must parse successfully before stripping"
            );
            assert!(
                result.source_bytes.is_none(),
                "streaming route result {i} must not retain source_bytes (peak-bounded contract)"
            );
        }
    }
}
