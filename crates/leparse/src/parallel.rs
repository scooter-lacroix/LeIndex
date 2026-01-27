// Parallel file parsing with rayon
//
// This module provides high-performance parallel parsing capabilities
// for processing multiple source files concurrently.

use crate::grammar::LanguageId;
use crate::languages::parser_for_language;
use crate::traits::SignatureInfo;
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
        parse_time_ms: u64,
    ) -> Self {
        Self {
            file_path,
            language: Some(language),
            signatures,
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

        // Detect language from file extension
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");

        let language_id = match LanguageId::from_extension(extension) {
            Some(id) => id,
            None => {
                let ext = extension.to_string();
                return ParsingResult::failure(
                    file_path,
                    format!("Unsupported file extension: {}", ext),
                )
            }
        };

        // Get language name for result
        let language_name = language_id.config().name.clone();

        // Read file contents
        let source = match std::fs::read(&file_path) {
            Ok(contents) => contents,
            Err(e) => {
                return ParsingResult::failure(
                    file_path,
                    format!("Failed to read file: {}", e),
                )
            }
        };

        // Get the language-specific parser factory
        let lang_parser = match parser_for_language(&language_name) {
            Some(p) => p,
            None => {
                return ParsingResult::failure(
                    file_path,
                    format!("No parser found for language: {}", language_name),
                )
            }
        };

        // Use thread-local pooled parser
        let result = THREAD_PARSER.with(|parser_cell| {
            let mut parser = parser_cell.borrow_mut();
            lang_parser.get_signatures_with_parser(&source, &mut parser)
        });

        // Process result
        let parse_time_ms = start_time.elapsed().as_millis() as u64;

        match result {
            Ok(signatures) => ParsingResult::success(file_path, language_name, signatures, parse_time_ms),
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
        assert!(stats.total_time_ms > 0);
    }
}
