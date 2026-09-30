//! Semantic drift detection for signature changes and API breakage

use crate::edit::{EditType, ResolvedEditChange};
use crate::graph::ProgramDependenceGraph;
use crate::graph::pdg::NodeType;
use crate::parse::go::GoParser;
use crate::parse::java::JavaParser;
use crate::parse::javascript::{JavaScriptParser, TypeScriptParser};
use crate::parse::python::PythonParser;
use crate::parse::rust::RustParser;
use crate::parse::traits::{CodeIntelligence, SignatureInfo};
use crate::validation::Location;
use crate::validation::ValidationError;
use std::collections::HashMap;
use std::sync::Arc;

/// Upper bound on memoised signature lists; the cache is cleared when full.
const SIGNATURE_CACHE_CAP: usize = 256;

static SIGNATURE_CACHE: std::sync::LazyLock<
    std::sync::Mutex<HashMap<[u8; 32], Arc<Vec<SignatureInfo>>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

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
    /// Symbol was renamed (old name removed, new name added, body
    /// otherwise structurally identical). Informational: a rename edit is
    /// remove+add by name *by definition*, so paired removal/addition under
    /// an `EditType::Rename` is the expected outcome, not drift.
    Renamed,
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
    pub fn type_changed(symbol_name: String, location: Location, type_desc: String) -> Self {
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

    /// Informational drift item for a paired rename (no error).
    pub fn renamed(old_name: String, new_name: String, location: Location) -> Self {
        Self {
            impact_description: format!("Symbol '{}' renamed to '{}'", old_name, new_name),
            symbol_name: new_name,
            drift_type: DriftType::Renamed,
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
        changes: &[ResolvedEditChange],
    ) -> Result<Vec<DriftItem>, ValidationError> {
        use rayon::prelude::*;

        // Each change parses two documents (before and after); a rename touches
        // several files. The parses are independent, so they run across cores:
        // this was 40% of a rename preview when done one after another.
        let per_change: Vec<Result<Vec<DriftItem>, ValidationError>> = changes
            .par_iter()
            .map(|change| {
                let (original, new) = rayon::join(
                    || self.extract_signatures(change, &change.original_content),
                    || self.extract_signatures(change, &change.new_content),
                );
                // Compare signatures to detect drift
                self.compare_signatures(change, &original?, &new?)
            })
            .collect();

        let mut drift_items = Vec::new();
        for items in per_change {
            drift_items.extend(items?);
        }
        Ok(drift_items)
    }

    /// Extract signatures from content, memoised by (language, content hash).
    ///
    /// The original file does not change between successive previews of the
    /// same edit, so repeat calls skip the parse entirely. Lite extraction is a
    /// pure function of `(language, bytes)`, which makes the key sound.
    fn extract_signatures(
        &self,
        change: &ResolvedEditChange,
        content: &str,
    ) -> Result<Arc<Vec<SignatureInfo>>, ValidationError> {
        if content.is_empty() {
            return Ok(Arc::new(Vec::new()));
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(change.infer_language().as_bytes());
        hasher.update(&[0]);
        hasher.update(content.as_bytes());
        let key: [u8; 32] = *hasher.finalize().as_bytes();

        if let Some(hit) = SIGNATURE_CACHE
            .lock()
            .ok()
            .and_then(|c| c.get(&key).cloned())
        {
            return Ok(hit);
        }
        let sigs = Arc::new(self.extract_signatures_uncached(change, content)?);
        if let Ok(mut cache) = SIGNATURE_CACHE.lock() {
            if cache.len() >= SIGNATURE_CACHE_CAP {
                cache.clear();
            }
            cache.insert(key, Arc::clone(&sigs));
        }
        Ok(sigs)
    }

    fn extract_signatures_uncached(
        &self,
        change: &ResolvedEditChange,
        content: &str,
    ) -> Result<Vec<SignatureInfo>, ValidationError> {
        let lang = change.infer_language();
        let source = content.as_bytes();
        let mut ts_parser = tree_sitter::Parser::new();

        // Drift reads only header fields, so use signature-only extraction
        // (no calls, flow facts, docstrings, imports or complexity). The
        // lite flag is thread-local and `get_signatures_lite` sets it on the
        // calling thread, which is the rayon worker running this closure.
        let (label, result) = match lang {
            "python" => (
                "Python",
                PythonParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            "javascript" => (
                "JavaScript",
                JavaScriptParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            "typescript" => (
                "TypeScript",
                TypeScriptParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            "rust" => (
                "Rust",
                RustParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            "go" => (
                "Go",
                GoParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            "java" => (
                "Java",
                JavaParser::new().get_signatures_lite(source, &mut ts_parser),
            ),
            // For unsupported languages, return empty
            _ => return Ok(Vec::new()),
        };
        result.map_err(|e| ValidationError::Parse(format!("Failed to parse {}: {}", label, e)))
    }

    /// Compare signatures to detect drift
    fn compare_signatures(
        &self,
        change: &ResolvedEditChange,
        original: &[SignatureInfo],
        new: &[SignatureInfo],
    ) -> Result<Vec<DriftItem>, ValidationError> {
        let mut drift_items = Vec::new();

        let original_map: HashMap<_, _> = original.iter().map(|sig| (&sig.name, sig)).collect();

        let new_map: HashMap<_, _> = new.iter().map(|sig| (&sig.name, sig)).collect();

        // Collect removals and additions by name.
        let mut removed: Vec<(&String, &SignatureInfo)> = original_map
            .keys()
            .filter(|name| !new_map.contains_key(*name))
            .map(|name| (*name, *original_map.get(*name).unwrap()))
            .collect();
        let mut added: Vec<(&String, &SignatureInfo)> = new_map
            .keys()
            .filter(|name| !original_map.contains_key(*name))
            .map(|name| (*name, *new_map.get(*name).unwrap()))
            .collect();

        // A rename edit is remove+add by name BY DEFINITION: before this
        // pairing, every `EditType::Rename` change produced a Removed(old)
        // + Added(new) pair, `has_errors()` classified the removal as an
        // error, and the rename tool hard-rejected its own output — apply
        // mode could never succeed. Under a rename edit, pair each removal
        // with a structurally identical addition (same parameter count,
        // return type, and method flag — i.e. the same symbol under a new
        // name) and report the pair as informational `Renamed`. Only
        // unpaired removals/additions remain as drift errors.
        if change.edit_type == EditType::Rename {
            let mut unpaired_removals: Vec<(&String, &SignatureInfo)> = Vec::new();
            for (old_name, old_sig) in removed.drain(..) {
                let pair_index = added.iter().position(|(_, new_sig)| {
                    new_sig.parameters.len() == old_sig.parameters.len()
                        && new_sig.return_type == old_sig.return_type
                        && new_sig.is_method == old_sig.is_method
                        && new_sig.is_async == old_sig.is_async
                });
                match pair_index {
                    Some(idx) => {
                        let (new_name, new_sig) = added.remove(idx);
                        let location = self.find_signature_location(change, new_sig);
                        drift_items.push(DriftItem::renamed(
                            old_name.to_string(),
                            new_name.to_string(),
                            location,
                        ));
                    }
                    None => unpaired_removals.push((old_name, old_sig)),
                }
            }
            // Anything still unpaired below keeps the legacy error semantics.
            removed = unpaired_removals;
        }

        // Check for removed symbols
        for (name, sig) in removed {
            let location = self.find_signature_location(change, sig);
            drift_items.push(DriftItem::removed(name.to_string(), location));
        }

        // Check for added symbols
        for (name, sig) in added {
            let location = self.find_signature_location(change, sig);
            drift_items.push(DriftItem::added(name.to_string(), location));
        }

        // Check for modified symbols
        for name in original_map.keys() {
            if let Some(new_sig) = new_map.get(name) {
                if let Some(original_sig) = original_map.get(name) {
                    if let Some(drift) =
                        self.detect_signature_drift(change, original_sig, new_sig)?
                    {
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
        change: &ResolvedEditChange,
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

        // Check for async changes: sync <-> async changes how callers must
        // invoke the symbol, so it is a real signature change.
        if original.is_async != new.is_async {
            return Ok(Some(DriftItem::signature_changed(
                new.name.clone(),
                location,
                if original.is_async { "async" } else { "sync" },
                if new.is_async { "async" } else { "sync" },
            )));
        }

        Ok(None)
    }

    /// Find the location of a signature in the edit change
    fn find_signature_location(
        &self,
        change: &ResolvedEditChange,
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
    }

    #[test]
    fn test_analyze_semantic_drift_empty_changes() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let analyzer = SemanticDriftAnalyzer::new(pdg);
        let changes: &[ResolvedEditChange] = &[];
        let result = analyzer.analyze_semantic_drift(changes).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_is_public_api() {
        let mut pdg = ProgramDependenceGraph::new();
        let _node_id = pdg.add_node(crate::graph::Node {
            id: "my_func".to_string(),
            node_type: NodeType::Function,
            name: "my_func".to_string(),
            file_path: Arc::from("test.py"),
            byte_range: (0, 10),
            complexity: 1,
            language: "python".to_string(),
        });

        let analyzer = SemanticDriftAnalyzer::new(Arc::new(pdg));
        assert!(analyzer.is_public_api("my_func"));
        assert!(!analyzer.is_public_api("nonexistent"));
    }

    #[test]
    fn test_find_signature_location() {
        let pdg = Arc::new(ProgramDependenceGraph::new());
        let analyzer = SemanticDriftAnalyzer::new(pdg);

        let change = ResolvedEditChange::new(
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
            visibility: crate::parse::traits::Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: vec![],
            imports: vec![],
            byte_range: (0, 14),
            flow_facts: vec![],

            cyclomatic_complexity: 0,
        };

        let location = analyzer.find_signature_location(&change, &sig);
        assert_eq!(location.line, 1);
        assert_eq!(location.column, 1);
    }
}

#[cfg(test)]
mod rename_pairing_tests {
    use super::*;
    use crate::edit::{EditType, ResolvedEditChange};
    use std::path::PathBuf;

    fn analyzer() -> SemanticDriftAnalyzer {
        SemanticDriftAnalyzer::new(std::sync::Arc::new(ProgramDependenceGraph::new()))
    }

    fn rename_change(original: &str, new: &str) -> ResolvedEditChange {
        ResolvedEditChange::new(
            PathBuf::from("fixture.rs"),
            original.to_string(),
            new.to_string(),
        )
        .with_edit_type(EditType::Rename)
    }

    #[test]
    fn test_rename_pairs_removed_and_added_instead_of_erroring() {
        // N-00 regression: a rename edit is remove+add by name by
        // definition. Before the pairing fix, this produced Removed(old) +
        // Added(new), `has_errors()` classified the removal as an error, and
        // rename-symbol apply mode hard-rejected every rename.
        let original = "pub fn stress_alpha(x: u32) -> u32 {\n    x + 1\n}\n";
        let new = "pub fn stress_alpha_prime(x: u32) -> u32 {\n    x + 1\n}\n";
        let items = analyzer()
            .analyze_semantic_drift(&[rename_change(original, new)])
            .unwrap();

        assert!(
            items
                .iter()
                .any(|item| item.drift_type == DriftType::Renamed),
            "a structurally identical rename must be reported as Renamed, got {:?}",
            items.iter().map(|i| &i.drift_type).collect::<Vec<_>>()
        );
        assert!(
            !items
                .iter()
                .any(|item| item.drift_type == DriftType::Removed),
            "paired removal must not surface as a Removed drift error"
        );
    }

    #[test]
    fn test_rename_with_structural_change_still_errors() {
        // A rename that ALSO changes the signature (extra parameter) is a
        // rename + signature change; the pairing must refuse to pair it and
        // keep the legacy Removed error so callers cannot silently change
        // signatures under the rename flag.
        let original = "pub fn stress_alpha(x: u32) -> u32 {\n    x + 1\n}\n";
        let new = "pub fn stress_alpha_prime(x: u32, y: u32) -> u32 {\n    x + y\n}\n";
        let items = analyzer()
            .analyze_semantic_drift(&[rename_change(original, new)])
            .unwrap();

        assert!(
            items
                .iter()
                .any(|item| item.drift_type == DriftType::Removed),
            "an unpaired removal must keep the legacy error semantics"
        );
    }

    #[test]
    fn test_replace_edit_still_reports_removals() {
        // Non-rename edits must be completely unaffected.
        let original = "pub fn stress_alpha(x: u32) -> u32 {\n    x + 1\n}\n";
        let new = "pub fn something_else(x: u32) -> u32 {\n    x + 1\n}\n";
        let change = ResolvedEditChange::new(
            PathBuf::from("fixture.rs"),
            original.to_string(),
            new.to_string(),
        );
        let items = analyzer().analyze_semantic_drift(&[change]).unwrap();

        assert!(
            items
                .iter()
                .any(|item| item.drift_type == DriftType::Removed)
        );
    }
}

#[cfg(test)]
mod async_drift_tests {
    use super::*;
    use crate::edit::ResolvedEditChange;
    use std::path::PathBuf;

    fn drift(path: &str, original: &str, new: &str) -> Vec<DriftItem> {
        let analyzer =
            SemanticDriftAnalyzer::new(std::sync::Arc::new(ProgramDependenceGraph::new()));
        let change =
            ResolvedEditChange::new(PathBuf::from(path), original.to_string(), new.to_string());
        analyzer.analyze_semantic_drift(&[change]).unwrap()
    }

    #[test]
    fn test_sync_to_async_is_signature_drift() {
        let items = drift(
            "fixture.rs",
            "pub fn load(x: u32) -> u32 {\n    x\n}\n",
            "pub async fn load(x: u32) -> u32 {\n    x\n}\n",
        );
        assert!(
            items
                .iter()
                .any(|i| i.symbol_name == "load" && i.drift_type == DriftType::SignatureChanged),
            "sync -> async must be reported as SignatureChanged, got {:?}",
            items.iter().map(|i| &i.drift_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_async_to_sync_is_signature_drift_python() {
        let items = drift(
            "fixture.py",
            "async def load(x):\n    return x\n",
            "def load(x):\n    return x\n",
        );
        assert!(
            items
                .iter()
                .any(|i| i.symbol_name == "load" && i.drift_type == DriftType::SignatureChanged),
            "async -> sync must be reported as SignatureChanged"
        );
    }

    #[test]
    fn test_unchanged_async_reports_no_drift() {
        let src = "pub async fn load(x: u32) -> u32 {\n    x\n}\n";
        assert!(drift("fixture.rs", src, src).is_empty());
    }
}
