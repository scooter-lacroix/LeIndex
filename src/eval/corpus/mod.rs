//! LeIndex evaluation corpus covering all spec section 9.2 categories.
//!
//! ## Categories
//!
//! The corpus covers all 14 retrieval categories from spec section 9.2:
//!
//! 1. NL-to-symbol: Natural-language intent to symbol
//! 2. Exact/partial identifier
//! 3. Concept to implementation
//! 4. Error/log string to origin
//! 5. Caller/callee/data-flow retrieval
//! 6. Interface to implementation
//! 7. Configuration/docs to code
//! 8. Similar algorithms with different names
//! 9. Same names with different behavior
//! 10. Changed/deleted files and freshness
//! 11. Large/generated distractors
//! 12. Multi-language (Rust/TS/Python/Go/Java/C/C++)
//! 13. Cross-language conceptual queries
//! 14. Hard negatives sharing syntax, names, or comments
//!
//! ## Splits
//!
//! The corpus has fixed train/eval splits. The eval split is the primary
//! evaluation surface; the train split is for tuning (if any).

pub mod data;
pub mod loader;

use serde::{Deserialize, Serialize};

// Re-export the category enum and corpus structures
pub use data::build_default_corpus;
pub use loader::load_and_verify_corpus;

/// All spec section 9.2 retrieval categories.
///
/// Each variant maps to a labeled category in the evaluation corpus.
/// The category name (via `as_str()`) is used as the key in metrics maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvalCategory {
    /// Natural-language intent to symbol.
    NlToSymbol,
    /// Exact and partial identifier search.
    ExactPartialIdentifier,
    /// Concept to implementation.
    ConceptToImpl,
    /// Error/log string to origin.
    ErrorLogToOrigin,
    /// Caller/callee/data-flow retrieval.
    CallerCalleeDataFlow,
    /// Interface to implementation.
    InterfaceToImpl,
    /// Configuration/docs to code.
    ConfigDocToCode,
    /// Similar algorithms with different names.
    SimilarAlgoDifferentName,
    /// Same names with different behavior.
    SameNameDifferentBehavior,
    /// Changed/deleted files and freshness.
    ChangedDeletedFreshness,
    /// Large files and generated-looking distractors.
    LargeGeneratedDistractors,
    /// Multi-language (Rust/TS/Python/Go/Java/C/C++).
    MultiLanguage,
    /// Cross-language conceptual queries.
    CrossLanguage,
    /// Hard negatives sharing syntax, names, or comments.
    HardNegatives,
}

impl EvalCategory {
    /// Returns the string identifier for this category.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NlToSymbol => "nl_to_symbol",
            Self::ExactPartialIdentifier => "exact_partial_identifier",
            Self::ConceptToImpl => "concept_to_impl",
            Self::ErrorLogToOrigin => "error_log_to_origin",
            Self::CallerCalleeDataFlow => "caller_callee_data_flow",
            Self::InterfaceToImpl => "interface_to_impl",
            Self::ConfigDocToCode => "config_doc_to_code",
            Self::SimilarAlgoDifferentName => "similar_algo_different_name",
            Self::SameNameDifferentBehavior => "same_name_different_behavior",
            Self::ChangedDeletedFreshness => "changed_deleted_freshness",
            Self::LargeGeneratedDistractors => "large_generated_distractors",
            Self::MultiLanguage => "multi_language",
            Self::CrossLanguage => "cross_language",
            Self::HardNegatives => "hard_negatives",
        }
    }

    /// Returns a human-readable description of this category.
    pub fn description(&self) -> &'static str {
        match self {
            Self::NlToSymbol => "Natural-language intent to symbol",
            Self::ExactPartialIdentifier => "Exact and partial identifier",
            Self::ConceptToImpl => "Concept to implementation",
            Self::ErrorLogToOrigin => "Error/log string to origin",
            Self::CallerCalleeDataFlow => "Caller/callee/data-flow retrieval",
            Self::InterfaceToImpl => "Interface to implementation",
            Self::ConfigDocToCode => "Configuration/docs to code",
            Self::SimilarAlgoDifferentName => "Similar algorithms with different names",
            Self::SameNameDifferentBehavior => "Same names with different behavior",
            Self::ChangedDeletedFreshness => "Changed/deleted files and freshness",
            Self::LargeGeneratedDistractors => "Large files and generated-looking distractors",
            Self::MultiLanguage => "Multi-language (Rust/TS/Python/Go/Java/C/C++)",
            Self::CrossLanguage => "Cross-language conceptual queries",
            Self::HardNegatives => "Hard negatives sharing syntax, names, or comments",
        }
    }

    /// Returns all 14 categories in canonical order.
    pub fn all() -> [EvalCategory; 14] {
        [
            Self::NlToSymbol,
            Self::ExactPartialIdentifier,
            Self::ConceptToImpl,
            Self::ErrorLogToOrigin,
            Self::CallerCalleeDataFlow,
            Self::InterfaceToImpl,
            Self::ConfigDocToCode,
            Self::SimilarAlgoDifferentName,
            Self::SameNameDifferentBehavior,
            Self::ChangedDeletedFreshness,
            Self::LargeGeneratedDistractors,
            Self::MultiLanguage,
            Self::CrossLanguage,
            Self::HardNegatives,
        ]
    }

    /// Parse a category from its string identifier.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "nl_to_symbol" => Some(Self::NlToSymbol),
            "exact_partial_identifier" => Some(Self::ExactPartialIdentifier),
            "concept_to_impl" => Some(Self::ConceptToImpl),
            "error_log_to_origin" => Some(Self::ErrorLogToOrigin),
            "caller_callee_data_flow" => Some(Self::CallerCalleeDataFlow),
            "interface_to_impl" => Some(Self::InterfaceToImpl),
            "config_doc_to_code" => Some(Self::ConfigDocToCode),
            "similar_algo_different_name" => Some(Self::SimilarAlgoDifferentName),
            "same_name_different_behavior" => Some(Self::SameNameDifferentBehavior),
            "changed_deleted_freshness" => Some(Self::ChangedDeletedFreshness),
            "large_generated_distractors" => Some(Self::LargeGeneratedDistractors),
            "multi_language" => Some(Self::MultiLanguage),
            "cross_language" => Some(Self::CrossLanguage),
            "hard_negatives" => Some(Self::HardNegatives),
            _ => None,
        }
    }
}

/// Which split a case belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Split {
    /// Training split (for tuning, not primary eval).
    Train,
    /// Evaluation split (primary eval surface).
    Eval,
}

/// Programming languages covered by the corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    /// Rust
    Rust,
    /// TypeScript
    TypeScript,
    /// JavaScript
    JavaScript,
    /// Python
    Python,
    /// Go
    Go,
    /// Java
    Java,
    /// C
    C,
    /// C++
    Cpp,
}

impl Language {
    /// Returns the string identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::C => "c",
            Self::Cpp => "cpp",
        }
    }
}

/// A single labeled evaluation case.
///
/// Each case has a query, a set of relevant symbols and files (labels),
/// the category it belongs to, and which split it's in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusCase {
    /// Unique identifier for this case.
    pub id: String,

    /// The query string a user or agent would issue.
    pub query: String,

    /// The retrieval category this case belongs to.
    pub category: EvalCategory,

    /// Which split (train or eval) this case is in.
    pub split: Split,

    /// Relevant symbol names (fully qualified or simple).
    /// These are the symbols the search should find.
    pub relevant_symbols: Vec<String>,

    /// Relevant file paths (relative to project root).
    /// These are the files containing the answer.
    pub relevant_files: Vec<String>,

    /// The language context for this case.
    pub languages: Vec<Language>,

    /// Optional: expected file position (line range) for the answer.
    pub expected_position: Option<(usize, usize)>,

    /// Optional: notes about the expected behavior or why this case is hard.
    pub notes: Option<String>,

    /// Optional: hard negative symbols/files that should NOT rank highly.
    /// These are syntactically or semantically similar but wrong.
    pub hard_negatives: Vec<String>,
}

/// The full evaluation corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corpus {
    /// All cases in the corpus.
    pub cases: Vec<CorpusCase>,
}

impl Corpus {
    /// Create a new empty corpus.
    pub fn new() -> Self {
        Self { cases: Vec::new() }
    }

    /// Add a case to the corpus.
    pub fn add_case(&mut self, case: CorpusCase) {
        self.cases.push(case);
    }

    /// Get all cases.
    pub fn all(&self) -> &[CorpusCase] {
        &self.cases
    }

    /// Get only the eval-split cases.
    pub fn eval_cases(&self) -> impl Iterator<Item = &CorpusCase> {
        self.cases.iter().filter(|c| c.split == Split::Eval)
    }

    /// Get only the train-split cases.
    pub fn train_cases(&self) -> impl Iterator<Item = &CorpusCase> {
        self.cases.iter().filter(|c| c.split == Split::Train)
    }

    /// Get cases for a specific category.
    pub fn cases_for_category(&self, cat: EvalCategory) -> impl Iterator<Item = &CorpusCase> {
        self.cases.iter().filter(move |c| c.category == cat)
    }

    /// Verify that all 14 categories are covered with minimum case counts.
    ///
    /// Returns Ok(()) if coverage is complete, or Err with a message listing
    /// missing/insufficient categories.
    pub fn verify_category_coverage(&self) -> Result<(), CorpusVerificationError> {
        let mut missing: Vec<String> = Vec::new();
        let mut insufficient: Vec<String> = Vec::new();
        const MIN_CASES_PER_CATEGORY: usize = 2;

        for cat in EvalCategory::all() {
            let count = self.cases_for_category(cat).count();
            if count == 0 {
                missing.push(format!("{}: 0 cases", cat.as_str()));
            } else if count < MIN_CASES_PER_CATEGORY {
                insufficient.push(format!(
                    "{}: {} cases (need {})",
                    cat.as_str(),
                    count,
                    MIN_CASES_PER_CATEGORY
                ));
            }
        }

        if !missing.is_empty() {
            return Err(CorpusVerificationError::MissingCategories(missing));
        }
        if !insufficient.is_empty() {
            return Err(CorpusVerificationError::InsufficientCases(insufficient));
        }
        Ok(())
    }

    /// Verify split integrity: every eval-split case has at least one
    /// relevant symbol or file, and train/eval splits don't share case IDs.
    pub fn verify_split_integrity(&self) -> Result<(), CorpusVerificationError> {
        let mut empty_labels: Vec<String> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut duplicate_ids: Vec<String> = Vec::new();

        for case in &self.cases {
            if case.relevant_symbols.is_empty() && case.relevant_files.is_empty() {
                empty_labels.push(case.id.clone());
            }
            if !seen_ids.insert(case.id.clone()) {
                duplicate_ids.push(case.id.clone());
            }
        }

        // Verify at least one eval case per category
        let mut eval_missing: Vec<String> = Vec::new();
        for cat in EvalCategory::all() {
            let has_eval = self.cases_for_category(cat).any(|c| c.split == Split::Eval);
            if !has_eval {
                eval_missing.push(cat.as_str().to_string());
            }
        }

        if !empty_labels.is_empty() {
            return Err(CorpusVerificationError::EmptyLabels(empty_labels));
        }
        if !duplicate_ids.is_empty() {
            return Err(CorpusVerificationError::DuplicateCaseIds(duplicate_ids));
        }
        if !eval_missing.is_empty() {
            return Err(CorpusVerificationError::NoEvalCasesForCategory(
                eval_missing,
            ));
        }
        Ok(())
    }

    /// Total number of cases.
    pub fn len(&self) -> usize {
        self.cases.len()
    }

    /// Number of eval cases.
    pub fn eval_len(&self) -> usize {
        self.eval_cases().count()
    }

    /// Number of train cases.
    pub fn train_len(&self) -> usize {
        self.train_cases().count()
    }

    /// Whether the corpus is empty.
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }
}

impl Default for Corpus {
    fn default() -> Self {
        Self::new()
    }
}

/// Error returned when corpus verification fails.
#[derive(Debug, Clone)]
pub enum CorpusVerificationError {
    /// Categories completely missing from the corpus.
    MissingCategories(Vec<String>),
    /// Categories with fewer than minimum cases.
    InsufficientCases(Vec<String>),
    /// Cases with no relevant files or symbols.
    EmptyLabels(Vec<String>),
    /// Duplicate case IDs.
    DuplicateCaseIds(Vec<String>),
    /// Categories with no eval-split cases.
    NoEvalCasesForCategory(Vec<String>),
}

impl std::fmt::Display for CorpusVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCategories(cats) => {
                write!(f, "Missing categories: {}", cats.join(", "))
            }
            Self::InsufficientCases(cats) => {
                write!(f, "Insufficient cases in: {}", cats.join(", "))
            }
            Self::EmptyLabels(ids) => write!(f, "Cases with empty labels: {}", ids.join(", ")),
            Self::DuplicateCaseIds(ids) => {
                write!(f, "Duplicate case IDs: {}", ids.join(", "))
            }
            Self::NoEvalCasesForCategory(cats) => {
                write!(f, "No eval cases for categories: {}", cats.join(", "))
            }
        }
    }
}

impl std::error::Error for CorpusVerificationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_all_returns_14() {
        let all = EvalCategory::all();
        assert_eq!(all.len(), 14);
    }

    #[test]
    fn test_category_roundtrip_strings() {
        for cat in EvalCategory::all() {
            let s = cat.as_str();
            let parsed = EvalCategory::from_str(s).expect("should parse");
            assert_eq!(parsed, cat);
        }
    }

    #[test]
    fn test_category_descriptions_nonempty() {
        for cat in EvalCategory::all() {
            assert!(!cat.description().is_empty());
        }
    }

    #[test]
    fn test_corpus_add_and_count() {
        let mut corpus = Corpus::new();
        assert_eq!(corpus.len(), 0);
        assert!(corpus.is_empty());

        corpus.add_case(CorpusCase {
            id: "test-1".to_string(),
            query: "find function".to_string(),
            category: EvalCategory::NlToSymbol,
            split: Split::Eval,
            relevant_symbols: vec!["my_func".to_string()],
            relevant_files: vec!["src/main.rs".to_string()],
            languages: vec![Language::Rust],
            expected_position: None,
            notes: None,
            hard_negatives: Vec::new(),
        });

        assert_eq!(corpus.len(), 1);
        assert!(!corpus.is_empty());
        assert_eq!(corpus.eval_len(), 1);
        assert_eq!(corpus.train_len(), 0);
    }

    #[test]
    fn test_corpus_split_filtering() {
        let mut corpus = Corpus::new();
        for i in 0..4 {
            corpus.add_case(CorpusCase {
                id: format!("case-{i}"),
                query: format!("query-{i}"),
                category: EvalCategory::NlToSymbol,
                split: if i % 2 == 0 {
                    Split::Eval
                } else {
                    Split::Train
                },
                relevant_symbols: vec!["sym".to_string()],
                relevant_files: vec!["file.rs".to_string()],
                languages: vec![Language::Rust],
                expected_position: None,
                notes: None,
                hard_negatives: Vec::new(),
            });
        }
        assert_eq!(corpus.eval_len(), 2);
        assert_eq!(corpus.train_len(), 2);
    }

    #[test]
    fn test_corpus_case_for_category() {
        let mut corpus = Corpus::new();
        corpus.add_case(CorpusCase {
            id: "1".to_string(),
            query: "q1".to_string(),
            category: EvalCategory::NlToSymbol,
            split: Split::Eval,
            relevant_symbols: vec!["s".to_string()],
            relevant_files: vec!["f.rs".to_string()],
            languages: vec![Language::Rust],
            expected_position: None,
            notes: None,
            hard_negatives: Vec::new(),
        });
        corpus.add_case(CorpusCase {
            id: "2".to_string(),
            query: "q2".to_string(),
            category: EvalCategory::ConceptToImpl,
            split: Split::Eval,
            relevant_symbols: vec!["s".to_string()],
            relevant_files: vec!["f.rs".to_string()],
            languages: vec![Language::Rust],
            expected_position: None,
            notes: None,
            hard_negatives: Vec::new(),
        });

        assert_eq!(
            corpus.cases_for_category(EvalCategory::NlToSymbol).count(),
            1
        );
        assert_eq!(
            corpus
                .cases_for_category(EvalCategory::ConceptToImpl)
                .count(),
            1
        );
        assert_eq!(
            corpus
                .cases_for_category(EvalCategory::HardNegatives)
                .count(),
            0
        );
    }
}
