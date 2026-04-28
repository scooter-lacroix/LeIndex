//! levalidation - Edit Validation Engine
//!
//! *Le Validation* (The Validation) - Comprehensive edit validation including
//! syntax checking via tree-sitter, reference integrity verification via legraphe,
//! semantic drift detection for signature changes, and impact analysis.

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

mod drift;
mod impact;
mod reference;
mod syntax;

pub use crate::edit::{EditType, ResolvedEditChange};
pub use drift::{DriftItem, DriftReport, DriftType, SemanticDriftAnalyzer};
pub use impact::{ImpactAnalyzer, ImpactReport, Location, RiskLevel};
pub use reference::{ReferenceChecker, ReferenceIssue, ReferenceIssueType};
pub use syntax::{ErrorSeverity, SyntaxError, SyntaxValidator};

use crate::graph::ProgramDependenceGraph;
use crate::storage::Storage;
use std::sync::Arc;
use thiserror::Error;

/// Result type for validation operations
pub type Result<T> = std::result::Result<T, ValidationError>;

/// Errors that can occur during validation
#[derive(Debug, Error)]
pub enum ValidationError {
    /// Storage error
    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),

    /// I/O error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Parse error
    #[error("Parse error: {0}")]
    Parse(String),

    /// Invalid edit change
    #[error("Invalid edit change: {0}")]
    InvalidEdit(String),

    /// Graph error
    #[error("Graph error: {0}")]
    Graph(String),
}

/// Comprehensive validation result
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether all validations passed
    pub is_valid: bool,
    /// Syntax errors found
    pub syntax_errors: Vec<SyntaxError>,
    /// Reference issues found
    pub reference_issues: Vec<ReferenceIssue>,
    /// Semantic drift items found
    pub semantic_drift: Vec<DriftItem>,
    /// Impact report
    pub impact_report: Option<ImpactReport>,
}

impl ValidationResult {
    /// Create a new empty validation result (passing)
    pub fn new() -> Self {
        Self {
            is_valid: true,
            syntax_errors: Vec::new(),
            reference_issues: Vec::new(),
            semantic_drift: Vec::new(),
            impact_report: None,
        }
    }

    /// Add a syntax error and mark as invalid
    pub fn add_syntax_error(&mut self, error: SyntaxError) {
        self.is_valid = false;
        self.syntax_errors.push(error);
    }

    /// Add a reference issue and mark as invalid
    pub fn add_reference_issue(&mut self, issue: ReferenceIssue) {
        self.is_valid = false;
        self.reference_issues.push(issue);
    }

    /// Add a semantic drift item and mark as invalid
    pub fn add_semantic_drift(&mut self, drift: DriftItem) {
        self.is_valid = false;
        self.semantic_drift.push(drift);
    }

    /// Set the impact report
    pub fn set_impact_report(&mut self, report: ImpactReport) {
        self.impact_report = Some(report);
    }

    /// Check if there are any errors (not warnings)
    pub fn has_errors(&self) -> bool {
        self.syntax_errors
            .iter()
            .any(|e| e.severity == ErrorSeverity::Error)
            || self.reference_issues.iter().any(|i| {
                matches!(
                    i.issue_type,
                    ReferenceIssueType::BrokenImport { .. }
                        | ReferenceIssueType::UndefinedReference { .. }
                )
            })
            || self.semantic_drift.iter().any(|d| {
                matches!(
                    d.drift_type,
                    DriftType::Removed | DriftType::SignatureChanged
                )
            })
    }
}

impl Default for ValidationResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Main validation engine that orchestrates all validation types
#[derive(Clone)]
pub struct LogicValidator {
    /// PDG for reference checking and impact analysis
    pdg: Arc<ProgramDependenceGraph>,
    /// Storage for reading source files
    storage: Arc<Storage>,
    /// Syntax validator
    syntax_validator: SyntaxValidator,
    /// Reference checker
    reference_checker: ReferenceChecker,
    /// Semantic drift analyzer
    drift_analyzer: SemanticDriftAnalyzer,
    /// Impact analyzer
    impact_analyzer: ImpactAnalyzer,
}

impl LogicValidator {
    /// Create a new LogicValidator
    ///
    /// # Arguments
    /// * `pdg` - Program Dependence Graph for reference checking
    /// * `storage` - Storage backend for reading source files
    pub fn new(pdg: Arc<ProgramDependenceGraph>, storage: Arc<Storage>) -> Self {
        Self {
            syntax_validator: SyntaxValidator::new(),
            reference_checker: ReferenceChecker::new(pdg.clone()),
            drift_analyzer: SemanticDriftAnalyzer::new(pdg.clone()),
            impact_analyzer: ImpactAnalyzer::new(pdg.clone()),
            pdg,
            storage,
        }
    }

    /// Validate a single edit change
    ///
    /// # Arguments
    /// * `change` - The edit change to validate
    ///
    /// # Returns
    /// Validation result with all found issues
    pub fn validate_change(&self, change: &ResolvedEditChange) -> Result<ValidationResult> {
        self.validate_changes(&[change.clone()])
    }

    /// Validate multiple edit changes
    ///
    /// # Arguments
    /// * `changes` - Slice of edit changes to validate
    ///
    /// # Returns
    /// Validation result with all found issues
    pub fn validate_changes(&self, changes: &[ResolvedEditChange]) -> Result<ValidationResult> {
        let mut result = ValidationResult::new();

        // Syntax validation
        for syntax_error in self.syntax_validator.validate_syntax(changes)? {
            result.add_syntax_error(syntax_error);
        }

        // Reference integrity checking
        for reference_issue in self.reference_checker.check_references(changes)? {
            result.add_reference_issue(reference_issue);
        }

        // Semantic drift detection
        for drift_item in self.drift_analyzer.analyze_semantic_drift(changes)? {
            result.add_semantic_drift(drift_item);
        }

        // Impact analysis
        let impact_report = self.impact_analyzer.analyze_impact(changes)?;
        result.set_impact_report(impact_report);

        Ok(result)
    }

    /// Get reference to the PDG
    pub fn pdg(&self) -> &Arc<ProgramDependenceGraph> {
        &self.pdg
    }

    /// Get reference to the storage
    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_validation_result_new() {
        let result = ValidationResult::new();
        assert!(result.is_valid);
        assert!(result.syntax_errors.is_empty());
        assert!(result.reference_issues.is_empty());
        assert!(result.semantic_drift.is_empty());
        assert!(result.impact_report.is_none());
    }

    #[test]
    fn test_validation_result_default() {
        let result = ValidationResult::default();
        assert!(result.is_valid);
    }

    #[test]
    fn test_validation_result_add_syntax_error() {
        let mut result = ValidationResult::new();
        let error = SyntaxError {
            file_path: PathBuf::from("test.py"),
            line: 1,
            column: 5,
            message: "Syntax error".to_string(),
            severity: ErrorSeverity::Error,
        };
        result.add_syntax_error(error);
        assert!(!result.is_valid);
        assert_eq!(result.syntax_errors.len(), 1);
        assert!(result.has_errors());
    }

    #[test]
    fn test_validation_result_add_reference_issue() {
        let mut result = ValidationResult::new();
        let issue = ReferenceIssue {
            issue_type: ReferenceIssueType::BrokenImport {
                symbol: "missing_module".to_string(),
            },
            file_path: PathBuf::from("test.py"),
            location: Location { line: 1, column: 1 },
            description: "Import not found".to_string(),
        };
        result.add_reference_issue(issue);
        assert!(!result.is_valid);
        assert_eq!(result.reference_issues.len(), 1);
        assert!(result.has_errors());
    }

    #[test]
    fn test_validation_result_add_semantic_drift() {
        let mut result = ValidationResult::new();
        let drift = DriftItem {
            symbol_name: "my_function".to_string(),
            drift_type: DriftType::SignatureChanged,
            location: Location { line: 5, column: 1 },
            impact_description: "Parameter type changed".to_string(),
        };
        result.add_semantic_drift(drift);
        assert!(!result.is_valid);
        assert_eq!(result.semantic_drift.len(), 1);
        assert!(result.has_errors());
    }

    #[test]
    fn test_validation_result_warning_only() {
        let mut result = ValidationResult::new();
        let warning = SyntaxError {
            file_path: PathBuf::from("test.py"),
            line: 1,
            column: 5,
            message: "Unused variable".to_string(),
            severity: ErrorSeverity::Warning,
        };
        result.add_syntax_error(warning);
        assert!(!result.is_valid); // Still invalid overall
        assert!(!result.has_errors()); // But no actual errors
    }

    #[test]
    fn test_location_display() {
        let loc = Location {
            line: 10,
            column: 5,
        };
        assert_eq!(loc.to_string(), "10:5");
    }

    #[test]
    fn test_validation_to_json_empty_result() {
        let result = ValidationResult::new();
        let json = validation_to_json(&result);

        // Verify all required fields present
        assert_eq!(json["is_valid"], true);
        assert_eq!(json["has_errors"], false);
        assert!(json["syntax_errors"].is_array());
        assert!(json["syntax_errors"].as_array().unwrap().is_empty());
        assert!(json["reference_issues"].is_array());
        assert!(json["reference_issues"].as_array().unwrap().is_empty());
        assert!(json["semantic_drift"].is_array());
        assert!(json["semantic_drift"].as_array().unwrap().is_empty());
        assert!(json["impact_report"].is_null());
    }

    #[test]
    fn test_validation_to_json_with_syntax_error() {
        let mut result = ValidationResult::new();
        result.add_syntax_error(SyntaxError {
            file_path: PathBuf::from("src/main.rs"),
            line: 42,
            column: 10,
            message: "expected `;`".to_string(),
            severity: ErrorSeverity::Error,
        });
        let json = validation_to_json(&result);

        assert_eq!(json["is_valid"], false);
        assert_eq!(json["has_errors"], true);

        let errors = json["syntax_errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["file"], "src/main.rs");
        assert_eq!(errors[0]["line"], 42);
        assert_eq!(errors[0]["column"], 10);
        assert_eq!(errors[0]["message"], "expected `;`");
        assert_eq!(errors[0]["severity"], "Error");
    }

    #[test]
    fn test_validation_to_json_with_reference_issue() {
        let mut result = ValidationResult::new();
        result.add_reference_issue(ReferenceIssue {
            issue_type: ReferenceIssueType::BrokenImport {
                symbol: "missing_mod".to_string(),
            },
            file_path: PathBuf::from("src/lib.rs"),
            location: Location { line: 5, column: 1 },
            description: "Import not found".to_string(),
        });
        let json = validation_to_json(&result);

        assert_eq!(json["is_valid"], false);
        assert_eq!(json["has_errors"], true);

        let issues = json["reference_issues"].as_array().unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0]["file"], "src/lib.rs");
        assert_eq!(issues[0]["location"], "5:1");
        assert_eq!(issues[0]["description"], "Import not found");
    }

    #[test]
    fn test_validation_to_json_with_semantic_drift() {
        let mut result = ValidationResult::new();
        result.add_semantic_drift(DriftItem {
            symbol_name: "my_func".to_string(),
            drift_type: DriftType::SignatureChanged,
            location: Location { line: 10, column: 1 },
            impact_description: "Parameter count changed".to_string(),
        });
        let json = validation_to_json(&result);

        assert_eq!(json["is_valid"], false);
        assert_eq!(json["has_errors"], true);

        let drift = json["semantic_drift"].as_array().unwrap();
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0]["symbol"], "my_func");
        assert_eq!(drift[0]["drift_type"], "SignatureChanged");
        assert_eq!(drift[0]["location"], "10:1");
        assert_eq!(drift[0]["impact"], "Parameter count changed");
    }

    #[test]
    fn test_validation_to_json_with_warning_only() {
        let mut result = ValidationResult::new();
        result.add_syntax_error(SyntaxError {
            file_path: PathBuf::from("test.rs"),
            line: 1,
            column: 1,
            message: "Unused variable".to_string(),
            severity: ErrorSeverity::Warning,
        });
        let json = validation_to_json(&result);

        assert_eq!(json["is_valid"], false);
        assert_eq!(json["has_errors"], false); // warnings only, no errors
        let errors = json["syntax_errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["severity"], "Warning");
    }
}

/// Convert a [`ValidationResult`] into a structured JSON value suitable for
/// inclusion in MCP edit responses.
///
/// The returned object always contains the following fields:
/// - `is_valid: bool`
/// - `has_errors: bool`
/// - `syntax_errors: []`
/// - `reference_issues: []`
/// - `semantic_drift: []`
/// - `impact_report: null | { risk_level, affected_symbols, affected_files }`
///
/// Empty arrays are produced for clean validations, ensuring a consistent
/// response shape that MCP consumers can rely on.
pub fn validation_to_json(result: &ValidationResult) -> serde_json::Value {
    let syntax_errors: Vec<serde_json::Value> = result
        .syntax_errors
        .iter()
        .map(|e| {
            serde_json::json!({
                "file": e.file_path.display().to_string(),
                "line": e.line,
                "column": e.column,
                "message": e.message,
                "severity": format!("{:?}", e.severity),
            })
        })
        .collect();

    let reference_issues: Vec<serde_json::Value> = result
        .reference_issues
        .iter()
        .map(|i| {
            serde_json::json!({
                "type": format!("{:?}", i.issue_type),
                "file": i.file_path.display().to_string(),
                "location": format!("{}:{}", i.location.line, i.location.column),
                "description": i.description,
            })
        })
        .collect();

    let semantic_drift: Vec<serde_json::Value> = result
        .semantic_drift
        .iter()
        .map(|d| {
            serde_json::json!({
                "symbol": d.symbol_name,
                "drift_type": format!("{:?}", d.drift_type),
                "location": format!("{}:{}", d.location.line, d.location.column),
                "impact": d.impact_description,
            })
        })
        .collect();

    let impact_report = result.impact_report.as_ref().map(|r| {
        serde_json::json!({
            "risk_level": format!("{:?}", r.risk_level),
            "affected_symbols": r.affected_nodes,
            "affected_files": r.affected_files.len(),
        })
    });

    serde_json::json!({
        "is_valid": result.is_valid,
        "has_errors": result.has_errors(),
        "syntax_errors": syntax_errors,
        "reference_issues": reference_issues,
        "semantic_drift": semantic_drift,
        "impact_report": impact_report,
    })
}

/// Library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
