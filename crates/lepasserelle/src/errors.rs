// Error Handling and Recovery
//
// *La Gestion des Erreurs* (The Error Management) - Comprehensive error types and recovery

use crate::config::ProjectConfig;
use anyhow::{anyhow, Error};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Result type for LeIndex operations
pub type Result<T> = std::result::Result<T, LeIndexError>;

/// LeIndex error types
#[derive(Debug)]
pub enum LeIndexError {
    /// Parsing-related errors
    #[error("Parsing error: {message}")]
    Parse {
        message: String,
        file_path: Option<PathBuf>,
        suggestion: Option<String>,
    },

    /// Indexing-related errors
    #[error("Indexing error: {message}")]
    Index {
        message: String,
        recoverable: bool,
    },

    /// Storage-related errors
    #[error("Storage error: {message}")]
    Storage {
        message: String,
        recoverable: bool,
    },

    /// Search-related errors
    #[error("Search error: {message}")]
    Search {
        message: String,
    },

    /// Configuration errors
    #[error("Configuration error: {message}")]
    Config {
        message: String,
        suggestion: Option<String>,
    },

    /// I/O errors with context
    #[error("I/O error: {context} (path: {path:?})")]
    Io {
        context: String,
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },

    /// Memory-related errors
    #[error("Memory error: {message}")]
    Memory {
        message: String,
        suggestion: Option<String>,
    },

    /// Validation errors
    #[error("Validation error: {message}")]
    Validation {
        message: String,
        suggestion: Option<String>,
    },
}

impl LeIndexError {
    /// Create a parse error with context
    pub fn parse_error(message: impl Into<String>, file_path: impl Into<PathBuf>) -> Self {
        LeIndexError::Parse {
            message: message.into(),
            file_path: Some(file_path.into()),
            suggestion: None,
        }
    }

    /// Create an indexing error
    pub fn index_error(message: impl Into<String>, recoverable: bool) -> Self {
        LeIndexError::Index {
            message: message.into(),
            recoverable,
        }
    }

    /// Create a storage error
    pub fn storage_error(message: impl Into<String>, recoverable: bool) -> Self {
        LeIndexError::Storage {
            message: message.into(),
            recoverable,
        }
    }

    /// Create a search error
    pub fn search_error(message: impl Into<String>) -> Self {
        LeIndexError::Search {
            message: message.into(),
        }
    }

    /// Create a config error
    pub fn config_error(message: impl Into<String>, suggestion: Option<String>) -> Self {
        LeIndexError::Config {
            message: message.into(),
            suggestion,
        }
    }

    /// Create a memory error
    pub fn memory_error(message: impl Into<String>, suggestion: Option<String>) -> Self {
        LeIndexError::Memory {
            message: message.into(),
            suggestion,
        }
    }

    /// Create a validation error
    pub fn validation_error(message: impl Into<String>, suggestion: Option<String>) -> Self {
        LeIndexError::Validation {
            message: message.into(),
            suggestion,
        }
    }

    /// Check if this error is recoverable
    pub fn is_recoverable(&self) -> bool {
        match self {
            LeIndexError::Index { recoverable, .. } => *recoverable,
            LeIndexError::Storage { recoverable, .. } => *recoverable,
            LeIndexError::Parse { .. } => true, // Parse errors are recoverable (skip file)
            _ => false,
        }
    }

    /// Get user-friendly suggestion for recovery
    pub fn suggestion(&self) -> Option<String> {
        match self {
            LeIndexError::Parse { suggestion, .. } => suggestion.clone(),
            LeIndexError::Config { suggestion, .. } => suggestion.clone(),
            LeIndexError::Memory { suggestion, .. } => suggestion.clone(),
            LeIndexError::Validation { suggestion, .. } => suggestion.clone(),
            LeIndexError::Index { recoverable: true, .. } => {
                Some("Try re-indexing with a smaller token budget or fewer languages.".to_string())
            }
            LeIndexError::Storage { recoverable: true, .. } => {
                Some("Try deleting .leindex directory and re-indexing.".to_string())
            }
            _ => None,
        }
    }
}

impl std::fmt::Display for LeIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            LeIndexError::Parse { message, .. } => write!(f, "Parse error: {}", message),
            LeIndexError::Index { message, .. } => write!(f, "Indexing error: {}", message),
            LeIndexError::Storage { message, .. } => write!(f, "Storage error: {}", message),
            LeIndexError::Search { message } => write!(f, "Search error: {}", message),
            LeIndexError::Config { message, .. } => write!(f, "Configuration error: {}", message),
            LeIndexError::Io { context, .. } => write!(f, "I/O error: {}", context),
            LeIndexError::Memory { message, .. } => write!(f, "Memory error: {}", message),
            LeIndexError::Validation { message, .. } => write!(f, "Validation error: {}", message),
        }
    }
}

/// Error recovery strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryStrategy {
    /// Skip the problematic item and continue
    Skip,

    /// Retry the operation
    Retry,

    /// Fall back to a simpler approach
    Fallback,

    /// Abort the operation
    Abort,
}

/// Error recovery context
#[derive(Debug, Clone)]
pub struct ErrorContext {
    /// Current operation being performed
    pub operation: String,

    /// File being processed (if applicable)
    pub file_path: Option<PathBuf>,

    /// Error that occurred
    pub error: LeIndexError,

    /// Number of errors encountered so far
    pub error_count: usize,

    /// Maximum errors before aborting
    pub max_errors: usize,
}

impl ErrorContext {
    /// Create a new error context
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            file_path: None,
            error: LeIndexError::validation_error("Unknown error", None),
            error_count: 0,
            max_errors: 100,
        }
    }

    /// Set the file path
    pub fn with_file_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.file_path = Some(path.into());
        self
    }

    /// Set the error
    pub fn with_error(mut self, error: LeIndexError) -> Self {
        self.error = error;
        self
    }

    /// Set maximum errors
    pub fn with_max_errors(mut self, max: usize) -> Self {
        self.max_errors = max;
        self
    }

    /// Determine recovery strategy based on error and context
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        // Always try to recover from parse errors
        if matches!(self.error, LeIndexError::Parse { .. }) {
            return RecoveryStrategy::Skip;
        }

        // For recoverable indexing/storage errors, retry once
        if self.error.is_recoverable() && self.error_count < 3 {
            return RecoveryStrategy::Retry;
        }

        // For validation errors, try fallback
        if matches!(self.error, LeIndexError::Validation { .. }) {
            return RecoveryStrategy::Fallback;
        }

        // For other errors after max errors, abort
        if self.error_count >= self.max_errors {
            return RecoveryStrategy::Abort;
        }

        // Default to skip for non-critical errors
        RecoveryStrategy::Skip
    }

    /// Check if we should abort based on error count
    pub fn should_abort(&self) -> bool {
        self.error_count >= self.max_errors
            || !self.error.is_recoverable()
    }
}

/// Partial indexing result
#[derive(Debug, Clone)]
pub struct PartialIndexResult {
    /// Files successfully processed
    pub successful_files: usize,

    /// Files that failed to process
    pub failed_files: Vec<PathBuf>,

    /// Partial statistics
    pub stats: PartialStats,

    /// Whether indexing completed successfully
    pub completed: bool,
}

/// Partial indexing statistics
#[derive(Debug, Clone)]
pub struct PartialStats {
    /// Total files encountered
    pub total_files: usize,

    /// Successfully parsed files
    pub parsed_files: usize,

    /// Total signatures extracted
    pub total_signatures: usize,

    /// Nodes indexed
    pub indexed_nodes: usize,
}

impl PartialIndexResult {
    /// Create a new partial result
    pub fn new() -> Self {
        Self {
            successful_files: 0,
            failed_files: Vec::new(),
            stats: PartialStats {
                total_files: 0,
                parsed_files: 0,
                total_signatures: 0,
                indexed_nodes: 0,
            },
            completed: false,
        }
    }

    /// Add a failed file
    pub fn add_failure(&mut self, file_path: PathBuf) {
        self.failed_files.push(file_path);
    }

    /// Check if indexing was successful enough to be usable
    pub fn is_usable(&self) -> bool {
        // Consider it usable if at least 50% of files were successful
        if self.stats.total_files == 0 {
            return false;
        }

        let success_rate = self.stats.parsed_files as f64 / self.stats.total_files as f64;
        success_rate >= 0.5
    }
}

impl Default for PartialIndexResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Corruption detection result
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorruptionStatus {
    /// No corruption detected
    Healthy,

    /// Minor corruption (some files missing)
    Minor {
        missing_files: usize,
    },

    /// Major corruption (index inconsistent)
    Major {
        description: String,
    },

    /// Severe corruption (requires rebuild)
    Severe {
        description: String,
    },
}

impl CorruptionStatus {
    /// Check if the index is healthy enough to use
    pub fn is_usable(&self) -> bool {
        match self {
            CorruptionStatus::Healthy => true,
            CorruptionStatus::Minor { .. } => true,
            CorruptionStatus::Major { .. } => false,
            CorruptionStatus::Severe { .. } => false,
        }
    }

    /// Get a user-friendly message about the corruption
    pub fn message(&self) -> String {
        match self {
            CorruptionStatus::Healthy => "Index is healthy".to_string(),
            CorruptionStatus::Minor { missing_files } => {
                format!("Minor corruption: {} files are missing", missing_files)
            }
            CorruptionStatus::Major { description } => {
                format!("Major corruption: {}", description)
            }
            CorruptionStatus::Severe { description } => {
                format!("Severe corruption: {}. Index rebuild required.", description)
            }
        }
    }
}

/// Detect corruption in the indexed data
///
/// # Arguments
///
/// * `project_path` - Path to the project directory
///
/// # Returns
///
/// `Result<CorruptionStatus>` - Corruption detection result
pub fn detect_corruption<P: AsRef<Path>>(project_path: P) -> Result<CorruptionStatus> {
    let project_path = project_path.as_ref();
    let leindex_dir = project_path.join(".leindex");

    if !leindex_dir.exists() {
        return Ok(CorruptionStatus::Healthy);
    }

    // Check if database exists
    let db_path = leindex_dir.join("leindex.db");
    if !db_path.exists() {
        return Ok(CorruptionStatus::Minor {
            missing_files: 1,
        });
    }

    // Try to open and validate the database
    // This is a simplified check - actual implementation would run queries
    match lestockage::schema::Storage::open(&db_path) {
        Ok(_) => Ok(CorruptionStatus::Healthy),
        Err(e) => {
            if e.to_string().contains("corrupted") {
                Ok(CorruptionStatus::Major {
                    description: format!("Database corruption detected: {}", e),
                })
            } else {
                Ok(CorruptionStatus::Severe {
                    description: format!("Cannot access database: {}", e),
                })
            }
        }
    }
}

/// Attempt to recover from corruption
///
/// # Arguments
///
/// * `project_path` - Path to the project directory
///
/// # Returns
///
/// `Result<bool>` - True if recovery was successful
pub fn recover_corruption<P: AsRef<Path>>(project_path: P) -> Result<bool> {
    let project_path = project_path.as_ref();
    let status = detect_corruption(project_path)?;

    match status {
        CorruptionStatus::Healthy => Ok(true),
        CorruptionStatus::Minor { .. } => {
            // Minor corruption - try to rebuild missing data
            Ok(false) // Would trigger re-index
        }
        CorruptionStatus::Major { .. } => {
            // Major corruption - delete and rebuild
            let leindex_dir = project_path.join(".leindex");
            fs::remove_dir_all(&leindex_dir)?;
            Ok(false) // Would trigger full re-index
        }
        CorruptionStatus::Severe { .. } => {
            // Severe corruption - recommend manual intervention
            Ok(false)
        }
    }
}

/// Format error for user display
///
/// # Arguments
///
/// * `error` - The error to format
///
/// # Returns
///
/// Formatted error message with suggestions
pub fn format_error(error: &LeIndexError) -> String {
    let mut message = format!("Error: {}", error);

    if let Some(suggestion) = error.suggestion() {
        message.push_str(&format!("\n\nSuggestion: {}", suggestion));
    }

    if let LeIndexError::Io { path, .. } = error {
        if let Some(p) = path {
            message.push_str(&format!("\n\nPath: {:?}", p));
        }
    }

    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let error = LeIndexError::parse_error("Test error", "/test/path");
        assert!(matches!(error, LeIndexError::Parse { .. }));
    }

    #[test]
    fn test_recoverable_check() {
        let error1 = LeIndexError::index_error("Test", true);
        assert!(error1.is_recoverable());

        let error2 = LeIndexError::index_error("Test", false);
        assert!(!error2.is_recoverable());
    }

    #[test]
    fn test_recovery_strategy() {
        let ctx = ErrorContext::new("test")
            .with_error(LeIndexError::parse_error("test", "/test/path"));

        let strategy = ctx.recovery_strategy();
        assert_eq!(strategy, RecoveryStrategy::Skip);
    }

    #[test]
    fn test_partial_result() {
        let mut result = PartialIndexResult::new();
        result.stats.total_files = 10;
        result.stats.parsed_files = 8;

        assert!(result.is_usable());

        result.stats.parsed_files = 4;
        assert!(!result.is_usable());
    }

    #[test]
    fn test_corruption_status() {
        let status = CorruptionStatus::Healthy;
        assert!(status.is_usable());

        let message = status.message();
        assert!(message.contains("healthy"));
    }
}
