//! Semantic drift detection for signature changes and API breakage

use crate::edit_change::EditChange;
use crate::ValidationError;
use crate::Location;
use legraphe::{ProgramDependenceGraph};
use legraphe::pdg::NodeType;
use leparse::traits::{CodeIntelligence, SignatureInfo};
use std::collections::HashMap;
use std::sync::Arc;

/// Type of semantic drift detected
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftType {
    /// Function/method signature changed
    SignatureChanged,
    /// Visibility modifier changed
    VisibilityChanged,
    /// Type changed (parameter or return type)
    TypeChanged,
    /// Symbol was removed
    Removed,
    /// Symbol was added
    Added,
}

/// A semantic drift item
#[derive(Debug, Clone)]
pub struct DriftItem {
    /// Name of the affected symbol
    pub symbol_name: String,
    /// Type of drift
    pub drift_type: DriftType,
    /// Location in source
    pub location: Location,
    /// Impact description
    pub impact_description: String,
}

impl DriftItem {
    /// Create a new drift item
    pub fn new(
        symbol_name: String,
        drift_type: DriftType,
        location: Location,
        impact_description: String,
    ) -> Self {
        Self {
            symbol_name,
            drift_type,
            location,
            impact_description,
        }
    }

    /// Create a signature changed drift
    pub fn signature_changed(
        symbol_name: String,
        location: Location,
        old_sig: &str,
        new_sig: &str,
    ) -> Self {
        Self {
            symbol_name,
            drift_type: DriftType::SignatureChanged,
            location,
            impact_description: format!("Signature changed from '{}' to '{}'", old_sig, new_sig),
        }
    }

    /// Create a type changed drift
    pub fn type_changed(
        symbol_name: String,
        location: Location,
        type_desc: String,
    ) -> Self {
        Self {
            symbol_name,
            drift_type: DriftType::TypeChanged,
            location,
            impact_description: format!("Type changed: {}", type_desc),
        }
    }

    /// Create a visibility changed drift
    pub fn visibility_changed(
        symbol_name: String,
        location: Location,
        old_visibility: &str,
        new_visibility: &str,
    ) -> Self {
        Self {
            symbol_name,
            drift_type: DriftType::VisibilityChanged,
            location,
            impact_description: format!(
                "Visibility changed from '{}' to '{}'",
                old_visibility, new_visibility
            ),
        }
    }

    /// Create a removed drift
    pub fn removed(symbol_name: String, location: Location) -> Self {
        Self {
            impact_description: format!("Symbol '{}' was removed", symbol_name),
            symbol_name,
            drift_type: DriftType::Removed,
            location,
        }
    }

    /// Create an added drift
    pub fn added(symbol_name: String, location: Location) -> Self {
        Self {
            impact_description: format!("New symbol '{}' added", symbol_name),
            symbol_name,
            drift_type: DriftType::Added,
            location,
        }
    }

    /// Check if this is a breaking change
    pub fn is_breaking(&self) -> bool {
        matches!(
            self.drift_type,
            DriftType::SignatureChanged | DriftType::TypeChanged | DriftType::Removed
        )
    }
}

/// Report of semantic drift analysis
#[derive(Debug, Clone)]
pub struct DriftReport {
    /// Breaking changes detected
    pub breaking_changes: Vec<DriftItem>,
    /// All API changes (breaking and non-breaking)
    pub api_changes: Vec<DriftItem>,
}

impl DriftReport {
    /// Create a new empty drift report
    pub fn new() -> Self {
        Self {
            breaking_changes: Vec::new(),
            api_changes: Vec::new(),
        }
    }

    /// Add a drift item to the report
    pub fn add_drift(&mut self, drift: DriftItem) {
        if drift.is_breaking() {
            self.breaking_changes.push(drift.clone());
        }
        self.api_changes.push(drift);
    }

    /// Check if there are any breaking changes
    pub fn has_breaking_changes(&self) -> bool {
        !self.breaking_changes.is_empty()
    }

    /// Get the count of all changes
    pub fn total_changes(&self) -> usize {
        self.api_changes.len()
    }
}

impl Default for DriftReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Semantic drift analyzer
#[derive(Clone)]
pub struct SemanticDriftAnalyzer {
    /// PDG for analyzing the codebase
    pdg: Arc<ProgramDependenceGraph>,
}

impl SemanticDriftAnalyzer {
    /// Create a new semantic drift analyzer
    pub fn new(pdg: Arc<ProgramDependenceGraph>) -> Self {
        Self { pdg }
    }

    /// Analyze semantic drift for edit changes
    ///
    /// # Arguments
    /// * `changes` - Edit changes to analyze
    ///
    /// # Returns
    /// Vector of drift items detected
    pub fn analyze_semantic_drift(
        &self,
        changes: &[EditChange],
    ) -> Result<Vec<DriftItem>, ValidationError> {
        let mut drift_items = Vec::new();

        for change in changes {
            // Extract signatures from original content
            let original_sigs = self.extract_signatures(change, &change.original_content)?;

            // Extract signatures from new content
            let new_sigs = self.extract_signatures(change, &change.new_content)?;

            // Compare signatures to detect drift
            drift_items.extend(self.compare_signatures(
                change,
                &original_sigs,
                &new_sigs,
            )?);
        }

        Ok(drift_items)
    }

    /// Extract signatures from content
    fn extract_signatures(
        &self,
        change: &EditChange,
        content: &str,
    ) -> Result<Vec<SignatureInfo>, ValidationError> {
        if content.is_empty() {
            return Ok(Vec::new());
        }

        let lang = change.infer_language();
        let source = content.as_bytes();

        // Get the appropriate parser for this language
        match lang {
            "python" => {
                use leparse::python::PythonParser;
                let parser = PythonParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse Python: {}", e)))
            }
            "javascript" => {
                use leparse::javascript::JavaScriptParser;
                let parser = JavaScriptParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse JavaScript: {}", e)))
            }
            "typescript" => {
                use leparse::javascript::TypeScriptParser;
                let parser = TypeScriptParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse TypeScript: {}", e)))
            }
            "rust" => {
                use leparse::rust::RustParser;
                let parser = RustParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse Rust: {}", e)))
            }
            "go" => {
                use leparse::go::GoParser;
                let parser = GoParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse Go: {}", e)))
            }
            "java" => {
                use leparse::java::JavaParser;
                let parser = JavaParser::new();
                parser.get_signatures(source)
                    .map_err(|e| ValidationError::Parse(format!("Failed to parse Java: {}", e)))
            }
            _ => {
                // For unsupported languages, return empty
                Ok(Vec::new())
            }
        }
    }

    /// Compare signatures to detect drift
    fn compare_signatures(
        &self,
        change: &EditChange,
        original: &[SignatureInfo],
        new: &[SignatureInfo],
    ) -> Result<Vec<DriftItem>, ValidationError> {
        let mut drift_items = Vec::new();

        let original_map: HashMap<_, _> = original
            .iter()
            .map(|sig| (&sig.name, sig))
            .collect();

        let new_map: HashMap<_, _> = new.iter().map(|sig| (&sig.name, sig)).collect();

        // Check for removed symbols
        for name in original_map.keys() {
            if !new_map.contains_key(name) {
                // Find location in original content
                let location = self.find_signature_location(change, original_map.get(name).unwrap());
                drift_items.push(DriftItem::removed(name.to_string(), location));
            }
        }

        // Check for added symbols
        for name in new_map.keys() {
            if !original_map.contains_key(name) {
                let location = self.find_signature_location(change, new_map.get(name).unwrap());
                drift_items.push(DriftItem::added(name.to_string(), location));
            }
        }

        // Check for modified symbols
        for name in original_map.keys() {
            if let Some(new_sig) = new_map.get(name) {
                if let Some(original_sig) = original_map.get(name) {
                    if let Some(drift) = self.detect_signature_drift(
                        change,
                        original_sig,
                        new_sig,
                    )? {
                        drift_items.push(drift);
                    }
                }
            }
        }

        Ok(drift_items)
    }

    /// Detect drift between two signatures
    fn detect_signature_drift(
        &self,
        change: &EditChange,
        original: &SignatureInfo,
        new: &SignatureInfo,
    ) -> Result<Option<DriftItem>, ValidationError> {
        let location = self.find_signature_location(change, new);

        // Check for signature changes (parameters)
        if original.parameters != new.parameters {
            return Ok(Some(DriftItem::signature_changed(
                new.name.clone(),
                location,
                &format!("{:?}", original.parameters),
                &format!("{:?}", new.parameters),
            )));
        }

        // Check for return type changes
        if original.return_type != new.return_type {
            return Ok(Some(DriftItem::type_changed(
                new.name.clone(),
                location,
                format!(
                    "Return type changed from {:?} to {:?}",
                    original.return_type, new.return_type
                ),
            )));
        }

        // Check for visibility changes
        if original.visibility != new.visibility {
            return Ok(Some(DriftItem::visibility_changed(
                new.name.clone(),
                location,
                &format!("{:?}", original.visibility),
                &format!("{:?}", new.visibility),
            )));
        }

        Ok(None)
    }

    /// Find the location of a signature in the edit change
    fn find_signature_location(
        &self,
        change: &EditChange,
        sig: &SignatureInfo,
    ) -> Location {
        let byte_offset = sig.byte_range.0;
        let mut line = 1;
        let mut column = 1;

        for (i, byte) in change.new_content.bytes().enumerate() {
            if i == byte_offset {
                break;
            }
            if byte == b'\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }

        Location { line, column }
    }

    /// Check if a symbol is part of the public API
    pub fn is_public_api(&self, symbol_name: &str) -> bool {
        if let Some(node_id) = self.pdg.find_by_symbol(symbol_name) {
            if let Some(node) = self.pdg.get_node(node_id) {
                // For now, consider all functions and classes as potential API
                // In a full implementation, this would check visibility modifiers
                return matches!(
                    node.node_type,
                    NodeType::Function | NodeType::Method | NodeType::Class
                );
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_drift_type_equality() {
        assert_eq!(DriftType::SignatureChanged, DriftType::SignatureChanged);
        assert_ne!(DriftType::SignatureChanged, DriftType::TypeChanged);
    }

    #[test]
    fn test_drift_item_signature_changed() {
        let item = DriftItem::signature_changed(
            "my_func".to_string(),
            Location { line: 1, column: 1 },
            "old_sig",
            "new_sig",
        );
        assert_eq!(item.symbol_name, "my_func");
        assert_eq!(item.drift_type, DriftType::SignatureChanged);
        assert!(item.impact_description.contains("old_sig"));
        assert!(item.impact_description.contains("new_sig"));
        assert!(item.is_breaking());
    }

    #[test]
    fn test_drift_item_type_changed() {
        let item = DriftItem::type_changed(
            "my_func".to_string(),
            Location { line: 1, column: 1 },
            "return type changed".to_string(),
        );
        assert_eq!(item.drift_type, DriftType::TypeChanged);
        assert!(item.is_breaking());
    }

    #[test]
    fn test_drift_item_visibility_changed() {
        let item = DriftItem::visibility_changed(
            "my_func".to_string(),
            Location { line: 1, column: 1 },
            "private",
            "public",
        );
        assert_eq!(item.drift_type, DriftType::VisibilityChanged);
        // Visibility changes are not considered breaking in this implementation
        assert!(!item.is_breaking());
    }

    #[test]
    fn test_drift_item_removed() {
        let item = DriftItem::removed("my_func".to_string(), Location { line: 1, column: 1 });
        assert_eq!(item.drift_type, DriftType::Removed);
        assert!(item.is_breaking());
        assert!(item.impact_description.contains("removed"));
    }

    #[test]
    fn test_drift_item_added() {
        let item = DriftItem::added("new_func".to_string(), Location { line: 1, column: 1 });
        assert_eq!(item.drift_type, DriftType::Added);
        assert!(!item.is_breaking()); // Adding is not breaking
        assert!(item.impact_description.contains("added"));
    }

    #[test]
    fn test_drift_report_new() {
        let report = DriftReport::new();
        assert!(report.breaking_changes.is_empty());
        assert!(report.api_changes.is_empty());
        assert!(!report.has_breaking_changes());
        assert_eq!(report.total_changes(), 0);
    }

    #[test]
    fn test_drift_report_default() {
        let report = DriftReport::default();
        assert!(report.breaking_changes.is_empty());
    }

    #[test]
    fn test_drift_report_add_drift() {
        let mut report = DriftReport::new();
        let item = DriftItem::removed("foo".to_string(), Location { line: 1, column: 1 });
        report.add_drift(item);
        assert_eq!(report.total_changes(), 1);
        assert!(report.has_breaking_changes());
        assert_eq!(report.breaking_changes.len(), 1);
    }

    #[test]
    fn test_drift_report_add_non_breaking() {
        let mut report = DriftReport::new();
        let item = DriftItem::added("foo".to_string(), Location { line: 1, column: 1 });
        report.add_drift(item);
        assert_eq!(report.total_changes(), 1);
        assert!(!report.has_breaking_changes());
        assert_eq!(report.breaking_changes.len(), 0);
    }

    #[test]
    fn test_semantic_drift_analyzer_new() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let _analyzer = SemanticDriftAnalyzer::new(pdg);
        // Just verify it was created
        assert!(true);
    }

    #[test]
    fn test_analyze_semantic_drift_empty_changes() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let analyzer = SemanticDriftAnalyzer::new(pdg);
        let changes: &[EditChange] = &[];
        let result = analyzer.analyze_semantic_drift(changes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_is_public_api() {
        let mut pdg = ProgramDependenceGraph::new();
        let _node_id = pdg.add_node(legraphe::Node {
            id: "my_func".to_string(),
            node_type: NodeType::Function,
            name: "my_func".to_string(),
            file_path: "test.py".to_string(),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
            embedding: None,
        });

        let analyzer = SemanticDriftAnalyzer::new(Arc::new(pdg));
        assert!(analyzer.is_public_api("my_func"));
        assert!(!analyzer.is_public_api("nonexistent"));
    }

    #[test]
    fn test_find_signature_location() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let analyzer = SemanticDriftAnalyzer::new(pdg);

        let change = EditChange::new(
            PathBuf::from("test.py"),
            String::new(),
            "def foo():\n    pass".to_string(),
        );

        // Create a signature at position 0
        let sig = SignatureInfo {
            name: "foo".to_string(),
            qualified_name: "foo".to_string(),
            parameters: vec![],
            return_type: None,
            visibility: leparse::traits::Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: vec![],
            imports: vec![],
            byte_range: (0, 14),
        };

        let location = analyzer.find_signature_location(&change, &sig);
        assert_eq!(location.line, 1);
        assert_eq!(location.column, 1);
    }
}
