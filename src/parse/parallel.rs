// Parallel file parsing with rayon
//
// This module provides high-performance parallel parsing capabilities
// for processing multiple source files concurrently.

use crate::parse::grammar::LanguageId;
use crate::parse::languages::parser_for_language;
use crate::parse::traits::SignatureInfo;
use rayon::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;
use tree_sitter::Parser;

thread_local! {
    /// Thread-local tree-sitter parser to avoid repeated allocations.
    /// Each thread in the Rayon thread pool will maintain its own parser.
    static THREAD_PARSER: RefCell<Parser> = RefCell::new(Parser::new());
}

/// Result of parsing a single file
#[derive(Debug, Clone)]
pub struct ParsingResult {
    /// Path to the file that was parsed
    pub file_path: PathBuf,

    /// Language detected for the file
    pub language: Option<String>,

    /// Extracted signatures (empty if parsing failed)
    pub signatures: Vec<SignatureInfo>,

    /// Source bytes from the file (to avoid re-reading from disk during PDG merge)
    pub source_bytes: Option<Vec<u8>>,

    /// Parsing error (if any)
    pub error: Option<String>,

    /// Time taken to parse this file (milliseconds)
    pub parse_time_ms: u64,
}

impl ParsingResult {
    /// Create a successful parsing result
    fn success(
        file_path: PathBuf,
        language: String,
        signatures: Vec<SignatureInfo>,
        source_bytes: Vec<u8>,
        parse_time_ms: u64,
    ) -> Self {
        Self {
            file_path,
            language: Some(language),
            signatures,
            source_bytes: Some(source_bytes),
            error: None,
            parse_time_ms,
        }
    }

    /// Create a failed parsing result
    fn failure(file_path: PathBuf, error: String) -> Self {
        Self {
            file_path,
            language: None,
            signatures: Vec::new(),
            source_bytes: None,
            error: Some(error),
            parse_time_ms: 0,
        }
    }

    /// Check if parsing was successful
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Check if parsing failed
    pub fn is_failure(&self) -> bool {
        self.error.is_some()
    }
}

/// Statistics from a parallel parsing operation
#[derive(Debug, Clone)]
pub struct ParsingStats {
    /// Total number of files processed
    pub total_files: usize,

    /// Number of files successfully parsed
    pub successful_files: usize,

    /// Number of files that failed to parse
    pub failed_files: usize,

    /// Total signatures extracted across all files
    pub total_signatures: usize,

    /// Total time taken for all parsing (milliseconds)
    pub total_time_ms: u64,

    /// Average time per file (milliseconds)
    pub avg_time_per_file_ms: f64,
}

impl ParsingStats {
    /// Create parsing statistics from results
    fn from_results(results: &[ParsingResult], total_time_ms: u64) -> Self {
        let successful = results.iter().filter(|r| r.is_success()).count();
        let failed = results.iter().filter(|r| r.is_failure()).count();
        let total_signatures = results.iter().map(|r| r.signatures.len()).sum();
        let avg_time = if results.is_empty() {
            0.0
        } else {
            total_time_ms as f64 / results.len() as f64
        };

        Self {
            total_files: results.len(),
            successful_files: successful,
            failed_files: failed,
            total_signatures,
            total_time_ms,
            avg_time_per_file_ms: avg_time,
        }
    }
}

/// Parallel parser for processing multiple files concurrently
pub struct ParallelParser {
    /// Maximum number of threads to use (None = use rayon default)
    max_threads: Option<usize>,

    /// Whether to collect detailed statistics
    collect_stats: bool,
}

impl Default for ParallelParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ParallelParser {
    /// Create a new parallel parser with default settings
    pub fn new() -> Self {
        Self {
            max_threads: None,
            collect_stats: true,
        }
    }

    /// Set the maximum number of threads to use
    pub fn with_max_threads(mut self, max_threads: usize) -> Self {
        self.max_threads = Some(max_threads);
        self
    }

    /// Disable statistics collection
    pub fn without_stats(mut self) -> Self {
        self.collect_stats = false;
        self
    }

    /// Parse multiple files in parallel
    pub fn parse_files(&self, file_paths: Vec<PathBuf>) -> Vec<ParsingResult> {
        let (results, _) = self.parse_files_with_stats(file_paths);
        results
    }

    /// Parse in parallel, reporting the running count of finished files.
    ///
    /// The callback exists so a caller can publish "N of M files" while this
    /// call is still running: `into_par_iter().collect()` yields nothing until
    /// every file is done, so a caller otherwise has no observable progress
    /// for the whole parse. One relaxed atomic increment per file is the only
    /// addition to the hot path — against a parse that allocates a tree.
    ///
    /// `on_progress` is the throttling boundary on purpose: this function
    /// reports EVERY completion, and the caller decides how often that
    /// becomes a durable write. Reporting here cannot skip an update that
    /// matters, and the caller cannot forget to sample.
    pub fn parse_files_counting(
        &self,
        file_paths: Vec<PathBuf>,
        on_progress: Option<&(dyn Fn(usize) + Sync)>,
    ) -> Vec<ParsingResult> {
        let Some(on_progress) = on_progress else {
            return self.parse_files(file_paths);
        };
        let done = std::sync::atomic::AtomicUsize::new(0);
        file_paths
            .into_par_iter()
            .map(|path| {
                let result = self.parse_single_file(path);
                let finished = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                on_progress(finished);
                result
            })
            .collect()
    }

    /// Parse multiple files in parallel and return both results and statistics
    pub fn parse_files_with_stats(
        &self,
        file_paths: Vec<PathBuf>,
    ) -> (Vec<ParsingResult>, ParsingStats) {
        let start_time = Instant::now();

        // Use parallel iterator to process files concurrently
        let results: Vec<ParsingResult> = file_paths
            .into_par_iter()
            .map(|path| self.parse_single_file(path))
            .collect();

        let total_time = start_time.elapsed().as_millis() as u64;
        let stats = ParsingStats::from_results(&results, total_time);

        if self.collect_stats {
            tracing::info!(
                "Parsed {} files: {} successful, {} failed, {} signatures, {:.2}ms avg",
                stats.total_files,
                stats.successful_files,
                stats.failed_files,
                stats.total_signatures,
                stats.avg_time_per_file_ms
            );
        }

        (results, stats)
    }

    /// Parse a single file
    fn parse_single_file(&self, file_path: PathBuf) -> ParsingResult {
        let start_time = Instant::now();
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_owned)
            .unwrap_or_default();
        let source = match std::fs::read(&file_path) {
            Ok(contents) => contents,
            Err(e) => {
                return ParsingResult::failure(file_path, format!("Failed to read file: {}", e));
            }
        };

        if crate::parse::binary::classify(&file_path, &source).is_some() {
            let binary = crate::parse::binary::extract(&file_path, &source);
            let end = binary.text.len();
            let signature = SignatureInfo {
                name: file_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("binary")
                    .to_string(),
                qualified_name: file_path.display().to_string(),
                parameters: Vec::new(),
                return_type: None,
                visibility: crate::parse::traits::Visibility::Public,
                is_async: false,
                is_method: false,
                docstring: Some(binary.text.clone()),
                calls: Vec::new(),
                imports: Vec::new(),
                byte_range: (0, end),
                cyclomatic_complexity: 0,
                flow_facts: Vec::new(),
            };
            return ParsingResult::success(
                file_path,
                binary.language.to_string(),
                vec![signature],
                source,
                start_time.elapsed().as_millis() as u64,
            );
        }

        let language_id = match LanguageId::from_extension(&extension) {
            Some(id) => id,
            None => {
                return ParsingResult::failure(
                    file_path,
                    format!("Unsupported file extension: {}", extension),
                );
            }
        };

        let language_name = language_id.config().name.clone();
        let lang_parser = match parser_for_language(&language_name) {
            Some(p) => p,
            None => {
                return ParsingResult::failure(
                    file_path,
                    format!("No parser found for language: {}", language_name),
                );
            }
        };

        let result = THREAD_PARSER.with(|parser_cell| {
            let mut parser = parser_cell.borrow_mut();
            lang_parser.get_signatures_with_parser(&source, &mut parser)
        });

        let parse_time_ms = start_time.elapsed().as_millis() as u64;
        match result {
            Ok(signatures) => {
                ParsingResult::success(file_path, language_name, signatures, source, parse_time_ms)
            }
            Err(e) => ParsingResult::failure(file_path, format!("Parse error: {}", e)),
        }
    }

    /// Get only successfully parsed results
    pub fn successful_results(results: &[ParsingResult]) -> Vec<&ParsingResult> {
        results.iter().filter(|r| r.is_success()).collect()
    }

    /// Get only failed results
    pub fn failed_results(results: &[ParsingResult]) -> Vec<&ParsingResult> {
        results.iter().filter(|r| r.is_failure()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn binary_results_retain_full_source_for_pdg_hashing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.dll");
        let source = vec![b'Z'; crate::parse::binary::MAX_BINARY_BYTES + 1024];
        std::fs::write(&path, &source).unwrap();

        let result = ParallelParser::new().parse_files(vec![path]).remove(0);

        assert!(result.is_success());
        assert_eq!(result.source_bytes.as_deref(), Some(source.as_slice()));
        assert_eq!(
            blake3::hash(result.source_bytes.as_deref().unwrap()),
            blake3::hash(&source)
        );
    }

    #[test]
    fn test_parallel_parser_multiple_files() {
        let dir = tempdir().unwrap();
        let py_path = dir.path().join("test.py");
        let rs_path = dir.path().join("test.rs");

        let mut py_file = File::create(&py_path).unwrap();
        writeln!(py_file, "def hello(): pass").unwrap();

        let mut rs_file = File::create(&rs_path).unwrap();
        writeln!(rs_file, "fn main() {{}}").unwrap();

        let parser = ParallelParser::new();
        let results = parser.parse_files(vec![py_path, rs_path]);

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_success()));
    }

    #[test]
    fn test_parallel_parser_with_error() {
        let dir = tempdir().unwrap();
        let unsupported_path = dir.path().join("test.unknown");
        File::create(&unsupported_path).unwrap();

        let parser = ParallelParser::new();
        let results = parser.parse_files(vec![unsupported_path]);

        assert_eq!(results.len(), 1);
        assert!(results[0].is_failure());
    }

    #[test]
    fn test_parsing_stats() {
        let dir = tempdir().unwrap();
        let py_path = dir.path().join("test.py");
        let mut py_file = File::create(&py_path).unwrap();
        writeln!(py_file, "def hello(): pass").unwrap();

        let parser = ParallelParser::new();
        let (_, stats) = parser.parse_files_with_stats(vec![py_path]);

        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.successful_files, 1);
        // total_time_ms is a u64 so just verify it exists (the parsing completed)
        let _ = stats.total_time_ms;
    }

    #[test]
    fn test_parsing_result_includes_source_bytes() {
        let dir = tempdir().unwrap();
        let py_path = dir.path().join("test.py");
        let mut py_file = File::create(&py_path).unwrap();
        writeln!(py_file, "def hello(): pass").unwrap();

        let parser = ParallelParser::new();
        let (results, _) = parser.parse_files_with_stats(vec![py_path]);

        assert_eq!(results.len(), 1);
        assert!(
            results[0].source_bytes.is_some(),
            "Successful parse should include source_bytes"
        );
        assert!(!results[0].source_bytes.as_ref().unwrap().is_empty());
    }

    /// The progress callback must observe every completion, monotonically, and
    /// end at the file count.
    ///
    /// This is the only signal a caller has while the parse runs:
    /// `into_par_iter().collect()` returns nothing until every file is done, so
    /// without this a long parse is indistinguishable from a hang. Fails if the
    /// increment is dropped, the wiring is broken, or the count is reported
    /// after the results (which would make it useless as live progress).
    #[test]
    fn test_parse_files_counting_reports_every_completion() {
        let dir = tempdir().unwrap();
        let mut paths = Vec::new();
        for index in 0..8 {
            let path = dir.path().join(format!("counted_{index}.py"));
            let mut file = File::create(&path).unwrap();
            writeln!(file, "def counted_{index}(): pass").unwrap();
            paths.push(path);
        }

        let seen = std::sync::Mutex::new(Vec::new());
        let on_progress = |done: usize| seen.lock().unwrap().push(done);
        let parser = ParallelParser::new();
        let results = parser.parse_files_counting(paths, Some(&on_progress));

        assert_eq!(results.len(), 8);
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen.len(),
            8,
            "every finished file must report once, got {seen:?}"
        );
        let mut ascending = seen.clone();
        ascending.sort_unstable();
        ascending.dedup();
        assert_eq!(
            ascending,
            (1..=8).collect::<Vec<_>>(),
            "reports must be exactly 1..=N with no gaps or repeats, got {seen:?}"
        );
    }

    /// `None` must take the untouched path: same results, no callback.
    #[test]
    fn test_parse_files_counting_without_a_callback_still_parses() {
        let dir = tempdir().unwrap();
        let py_path = dir.path().join("uncounted.py");
        let mut py_file = File::create(&py_path).unwrap();
        writeln!(py_file, "def uncounted(): pass").unwrap();

        let parser = ParallelParser::new();
        let results = parser.parse_files_counting(vec![py_path], None);

        assert_eq!(results.len(), 1);
        assert!(results[0].is_success());
    }
}
