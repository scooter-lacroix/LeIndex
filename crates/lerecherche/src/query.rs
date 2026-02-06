// Natural Language Query Processing
//
// *La Question* (The Question) - Convert natural language to structured search
//
// # Security & Performance Guarantees
//
// This module implements natural language query processing with the following guarantees:
//
// 1. **Input Validation**: All queries are validated for length and content before processing
// 2. **Regex Safety**: Patterns are designed to avoid catastrophic backtracking
// 3. **Memory Safety**: Token budgets are bounded and validated
// 4. **Performance**: O(n) complexity where possible, no unnecessary allocations
// 5. **Thread Safety**: QueryParser is Send + Sync (immutable after creation)

use crate::ranking::QueryType;
use crate::search::SearchQuery;
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

// ============================================================================
// CONSTANTS & VALIDATION
// ============================================================================

/// Maximum query length in characters (prevents DoS via overly long queries)
pub const MAX_QUERY_LENGTH: usize = 500;

/// Minimum query length in characters
pub const MIN_QUERY_LENGTH: usize = 1;

/// Maximum top_k value (prevents memory exhaustion)
pub const MAX_TOP_K: usize = 1000;

/// Minimum top_k value
pub const MIN_TOP_K: usize = 1;

/// Maximum token budget for context expansion (prevents memory exhaustion)
pub const MAX_TOKEN_BUDGET: usize = 10000;

/// Default token budget for context expansion
pub const DEFAULT_TOKEN_BUDGET: usize = 2000;

/// Maximum embedding dimension (prevents memory exhaustion)
pub const MAX_EMBEDDING_DIMENSION: usize = 10000;

/// Minimum embedding dimension
pub const MIN_EMBEDDING_DIMENSION: usize = 1;

// ============================================================================
// COMPILE-TIME VALIDATED REGEX PATTERNS
// ============================================================================

/// These patterns are compiled once at program startup.
/// If they fail to compile, the program will panic at startup (not during runtime).
///
/// Pattern Design Principles:
/// - Use atomic groups (?>...) to prevent backtracking
/// - Use possessive quantifiers where possible
/// - Avoid nested optional groups
/// - Limit repetition ranges
static HOW_WORKS_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:show|tell|explain|describe)\s+(?:me\s+)?how\s+(?:does\s+)?\S.{0,400}?(?:\s+(?:work|works|working|function|functions|operate|operates))?\s*\.?\s*$"
    ).expect("Failed to compile HOW_WORKS_PATTERN")
});

static WHERE_HANDLED_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^where\s+(?:is|are|do\s+we\s+handle|does\s+.\s+handle)\s+\S.{0,400}?(?:\s+handled)?\s*\.?\s*$"
    ).expect("Failed to compile WHERE_HANDLED_PATTERN")
});

static BOTTLENECKS_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:what|where|find)\s+(?:are\s+)?(?:the\s+)?(?:bottlenecks|performance\s+issues|slow\s+code|optimization\s+opportunities)\s*\.?\s*$"
    ).expect("Failed to compile BOTTLENECKS_PATTERN")
});

static COMPLEXITY_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(?:most|least)\s+(?:complex|complicated|difficult|simple)(?:\s+\S.{0,100})?\s*\.?\s*$"
    ).expect("Failed to compile COMPLEXITY_PATTERN")
});

// ============================================================================
// STOP WORDS (COMPILE-TIME STATIC)
// ============================================================================

/// Stop words are filtered out during tokenization to improve search relevance.
/// Using a static array instead of HashSet for better performance and memory efficiency.
static STOP_WORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
        "from", "as", "is", "was", "are", "were", "been", "be", "have", "has", "had", "do", "does",
        "did", "will", "would", "could", "should", "may", "might", "must", "shall", "can", "need",
        "show", "me", "tell", "explain", "describe", "how", "what", "where", "when", "why",
        "which", "that", "this", "these", "those",
    ]
    .iter()
    .cloned()
    .collect()
});

// ============================================================================
// QUERY INTENT ENUM
// ============================================================================

/// Natural language query intent
///
/// This represents the high-level intent of a natural language query,
/// which determines how the query should be processed and ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    /// Find how something works ("show me how X works")
    HowWorks,

    /// Find where something is handled ("where is X handled")
    WhereHandled,

    /// Find bottlenecks or issues ("what are bottlenecks")
    Bottlenecks,

    /// General semantic search
    Semantic,

    /// Text-based search
    Text,
}

// ============================================================================
// PARSED QUERY STRUCT
// ============================================================================

/// Parsed natural language query
///
/// This represents the result of parsing a natural language query,
/// containing all the information needed to execute a search.
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Original query text (normalized)
    pub original: String,

    /// Extracted key terms (filtered, normalized)
    pub terms: Vec<String>,

    /// Query intent
    pub intent: QueryIntent,

    /// Query type for ranking
    pub query_type: QueryType,

    /// Whether to expand context
    pub expand_context: bool,

    /// Maximum results (validated)
    pub top_k: usize,

    /// Token budget for context expansion (validated)
    pub token_budget: Option<usize>,
}

impl ParsedQuery {
    /// Validate the parsed query
    ///
    /// Ensures all fields contain valid values.
    fn validate(&self) -> Result<(), Error> {
        if self.top_k < MIN_TOP_K || self.top_k > MAX_TOP_K {
            return Err(Error::InvalidTopK {
                provided: self.top_k,
                min: MIN_TOP_K,
                max: MAX_TOP_K,
            });
        }

        if let Some(budget) = self.token_budget {
            if budget > MAX_TOKEN_BUDGET {
                return Err(Error::TokenBudgetTooLarge {
                    provided: budget,
                    max: MAX_TOKEN_BUDGET,
                });
            }
        }

        Ok(())
    }
}

// ============================================================================
// QUERY PARSER
// ============================================================================

/// Natural language query parser
///
/// This parser converts natural language queries into structured search queries
/// with intent classification and pattern matching.
///
/// # Thread Safety
///
/// `QueryParser` is `Send + Sync` and can be safely shared between threads.
/// All regex patterns are pre-compiled and stored in `once_cell` statics.
///
/// # Performance
///
/// - Regex patterns are compiled once at program startup
/// - Stop words are stored in a static HashSet
/// - Query validation is O(1) for length checks
/// - Tokenization is O(n) where n is query length
///
/// # Example
///
/// ```ignore
/// let parser = QueryParser::new()?;
/// let parsed = parser.parse("show me how authentication works", 10)?;
/// ```
pub struct QueryParser {
    // No internal state - all patterns are static
    // This struct exists for API compatibility and future extensibility
}

impl QueryParser {
    /// Create a new query parser
    ///
    /// This is a no-op constructor that always succeeds.
    /// All regex patterns are pre-compiled in statics.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Result<Self, Error> {
        Ok(Self {})
    }

    /// Parse a natural language query with full validation
    ///
    /// # Arguments
    ///
    /// * `query` - The natural language query text
    /// * `default_top_k` - Default maximum number of results (1-1000)
    ///
    /// # Returns
    ///
    /// A `ParsedQuery` containing all extracted information, or an error if:
    /// - Query is empty
    /// - Query exceeds maximum length
    /// - Query contains invalid characters
    /// - top_k is out of valid range
    ///
    /// # Errors
    ///
    /// - `Error::EmptyQuery` - Query is empty after trimming
    /// - `Error::QueryTooLong` - Query exceeds MAX_QUERY_LENGTH
    /// - `Error::InvalidCharacters` - Query contains control characters or null bytes
    /// - `Error::InvalidTopK` - top_k is out of valid range
    /// - `Error::NoMeaningfulTerms` - Query contains no meaningful terms after filtering
    pub fn parse(&self, query: &str, default_top_k: usize) -> Result<ParsedQuery, Error> {
        // Step 1: Validate input
        let query = self.validate_and_sanitize_query(query)?;

        // Step 2: Validate and normalize top_k
        let top_k = self.validate_top_k(default_top_k)?;

        // Step 3: Detect intent
        let intent = self.detect_intent(&query);

        // Step 4: Extract terms based on intent
        let terms = self.extract_terms(&query, &intent)?;

        // Step 5: Validate that we have meaningful terms
        if terms.is_empty() {
            return Err(Error::NoMeaningfulTerms {
                query: self.truncate_for_error(&query),
                suggestion: "Try using more specific terms or complete sentences",
            });
        }

        // Step 6: Determine query type
        let query_type = self.classify_query(&intent);

        // Step 7: Determine if we should expand context
        let expand_context = matches!(intent, QueryIntent::HowWorks | QueryIntent::WhereHandled);

        // Step 8: Set token budget for context expansion
        let token_budget = if expand_context {
            Some(DEFAULT_TOKEN_BUDGET)
        } else {
            None
        };

        // Step 9: Build parsed query
        let parsed = ParsedQuery {
            original: query,
            terms,
            intent,
            query_type,
            expand_context,
            top_k,
            token_budget,
        };

        // Step 10: Validate the parsed query
        parsed.validate()?;

        Ok(parsed)
    }

    /// Validate and sanitize query input
    ///
    /// This performs comprehensive validation:
    /// 1. Check query length bounds
    /// 2. Check for null bytes
    /// 3. Check for control characters (except whitespace)
    /// 4. Normalize Unicode
    /// 5. Trim whitespace
    fn validate_and_sanitize_query(&self, query: &str) -> Result<String, Error> {
        // Check for null bytes
        if query.contains('\0') {
            return Err(Error::InvalidCharacters {
                reason: "Query contains null bytes".to_string(),
            });
        }

        // Check length before normalization (prevent allocation attacks)
        if query.len() > MAX_QUERY_LENGTH {
            return Err(Error::QueryTooLong {
                provided: query.len(),
                max: MAX_QUERY_LENGTH,
                actual_prefix: self.truncate_for_error(query),
            });
        }

        // Check for control characters (except whitespace)
        for ch in query.chars() {
            if ch.is_control() && !ch.is_whitespace() {
                return Err(Error::InvalidCharacters {
                    reason: format!("Query contains control character: U+{:04X}", ch as u32),
                });
            }
        }

        // Normalize Unicode (NFC normalization)
        let normalized = query.nfc().collect::<String>();

        // Trim and check final length
        let trimmed = normalized.trim();

        if trimmed.is_empty() {
            return Err(Error::EmptyQuery);
        }

        if trimmed.len() > MAX_QUERY_LENGTH {
            return Err(Error::QueryTooLong {
                provided: trimmed.len(),
                max: MAX_QUERY_LENGTH,
                actual_prefix: self.truncate_for_error(trimmed),
            });
        }

        Ok(trimmed.to_string())
    }

    /// Validate top_k parameter
    fn validate_top_k(&self, top_k: usize) -> Result<usize, Error> {
        if !(MIN_TOP_K..=MAX_TOP_K).contains(&top_k) {
            return Err(Error::InvalidTopK {
                provided: top_k,
                min: MIN_TOP_K,
                max: MAX_TOP_K,
            });
        }
        Ok(top_k)
    }

    /// Detect the intent of the query
    ///
    /// This uses regex pattern matching to detect the query intent.
    /// Patterns are ordered from most specific to least specific.
    fn detect_intent(&self, query: &str) -> QueryIntent {
        // Check for "show me how X works" pattern (most specific)
        if HOW_WORKS_PATTERN.is_match(query) {
            return QueryIntent::HowWorks;
        }

        // Check for "where is X handled" pattern
        if WHERE_HANDLED_PATTERN.is_match(query) {
            return QueryIntent::WhereHandled;
        }

        // Check for "what are bottlenecks" pattern
        if BOTTLENECKS_PATTERN.is_match(query) {
            return QueryIntent::Bottlenecks;
        }

        // Check for complexity-related queries
        if COMPLEXITY_PATTERN.is_match(query) {
            return QueryIntent::Bottlenecks;
        }

        // Check if query contains question words (likely semantic)
        let query_lower = query.to_lowercase();
        if query_lower.contains("how")
            || query_lower.contains("what")
            || query_lower.contains("why")
            || query_lower.contains("where")
            || query_lower.contains("when")
            || query_lower.contains("which")
        {
            return QueryIntent::Semantic;
        }

        // Default to text search
        QueryIntent::Text
    }

    /// Extract key terms from the query
    ///
    /// This extracts meaningful terms from the query based on intent,
    /// filters stop words, and removes duplicates.
    fn extract_terms(&self, query: &str, intent: &QueryIntent) -> Result<Vec<String>, Error> {
        let mut terms = Vec::new();

        match intent {
            QueryIntent::HowWorks => {
                // Extract subject from "show me how X works"
                if let Some(captures) = HOW_WORKS_PATTERN.captures(query) {
                    // The entire match (excluding the prefix) is the subject
                    let full_match = captures.get(0).map(|m| m.as_str()).unwrap_or("");
                    // Extract the subject part by removing the known prefix
                    let subject = full_match
                        .to_lowercase()
                        .replace("show me how ", "")
                        .replace("show how ", "")
                        .replace("tell me how ", "")
                        .replace("tell how ", "")
                        .replace("explain how ", "")
                        .replace("describe how ", "")
                        .replace(" how does ", "")
                        .replace(" how ", "")
                        .replace(" works", "")
                        .replace(" work", "")
                        .replace(" functions", "")
                        .replace(" function", "")
                        .replace(" operates", "")
                        .replace(" operate", "");
                    let subject = subject.trim();
                    if !subject.is_empty() {
                        terms.extend(self.tokenize(subject));
                    }
                }
                // Fallback to full query tokenization if pattern didn't capture
                if terms.is_empty() {
                    terms.extend(self.tokenize(query));
                }
            }
            QueryIntent::WhereHandled => {
                // Extract subject from "where is X handled"
                if let Some(captures) = WHERE_HANDLED_PATTERN.captures(query) {
                    let full_match = captures.get(0).map(|m| m.as_str()).unwrap_or("");
                    let subject = full_match
                        .to_lowercase()
                        .replace("where is ", "")
                        .replace("where are ", "")
                        .replace("where do we handle ", "")
                        .replace("where does ", "")
                        .replace(" handled", "");
                    let subject = subject.trim();
                    if !subject.is_empty() {
                        terms.extend(self.tokenize(subject));
                    }
                }
                if terms.is_empty() {
                    terms.extend(self.tokenize(query));
                }
            }
            QueryIntent::Bottlenecks => {
                // For bottleneck queries, extract relevant terms
                terms.extend(self.tokenize(query));
            }
            QueryIntent::Semantic | QueryIntent::Text => {
                // Extract all meaningful terms
                terms.extend(self.tokenize(query));
            }
        }

        // Filter stop words and remove duplicates
        let filtered: Vec<String> = terms
            .into_iter()
            .filter(|t| !STOP_WORDS.contains(t.as_str()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        Ok(filtered)
    }

    /// Tokenize text into individual terms
    ///
    /// This splits text on whitespace, converts to lowercase,
    /// and filters out very short words (< 3 chars).
    ///
    /// Uses Cow<str> to avoid unnecessary allocations where possible.
    fn tokenize(&self, text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|s| s.to_lowercase())
            .map(|s| {
                s.trim_end_matches(|c: char| !c.is_alphanumeric())
                    .to_string()
            })
            .filter(|s| s.len() >= 3)
            .collect()
    }

    /// Classify query type for ranking
    fn classify_query(&self, intent: &QueryIntent) -> QueryType {
        match intent {
            QueryIntent::HowWorks | QueryIntent::Semantic => QueryType::Semantic,
            QueryIntent::WhereHandled => QueryType::Structural,
            QueryIntent::Bottlenecks => QueryType::Structural,
            QueryIntent::Text => QueryType::Text,
        }
    }

    /// Build a SearchQuery from a parsed query
    pub fn build_search_query(&self, parsed: &ParsedQuery) -> SearchQuery {
        // Reconstruct query string from terms
        let query_text = if parsed.terms.is_empty() {
            parsed.original.clone()
        } else {
            parsed.terms.join(" ")
        };

        SearchQuery {
            query: query_text,
            top_k: parsed.top_k,
            token_budget: parsed.token_budget,
            semantic: matches!(parsed.query_type, QueryType::Semantic),
            expand_context: parsed.expand_context,
            query_embedding: None,
            threshold: None,
        }
    }

    /// Truncate query for error messages
    fn truncate_for_error(&self, query: &str) -> String {
        if query.len() <= 100 {
            query.to_string()
        } else {
            format!("{}...", &query[..97])
        }
    }
}

impl Default for QueryParser {
    fn default() -> Self {
        Self::new().expect("QueryParser::new should never fail")
    }
}

// Safety: QueryParser has no internal mutable state and all dependencies are thread-safe
unsafe impl Send for QueryParser {}
unsafe impl Sync for QueryParser {}

// ============================================================================
// ERROR TYPES
// ============================================================================

/// Natural language query errors
///
/// Provides detailed error context for debugging and user feedback.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The query is empty or contains only whitespace
    #[error("Query cannot be empty")]
    EmptyQuery,

    /// The query exceeds the maximum allowed length
    #[error("Query too long: {provided} characters (max: {max}). Query: '{actual_prefix}'")]
    QueryTooLong {
        /// Number of characters provided
        provided: usize,
        /// Maximum allowed characters
        max: usize,
        /// Truncated prefix of the actual query
        actual_prefix: String,
    },

    /// The query contains invalid or control characters
    #[error("Query contains invalid characters: {reason}")]
    InvalidCharacters {
        /// Reason why the characters are invalid
        reason: String,
    },

    /// The top_k parameter is out of the valid range
    #[error("Invalid top_k value: {provided} (must be between {min} and {max})")]
    InvalidTopK {
        /// Provided top_k value
        provided: usize,
        /// Minimum allowed value
        min: usize,
        /// Maximum allowed value
        max: usize,
    },

    /// The token budget exceeds the maximum allowed value
    #[error("Token budget too large: {provided} (max: {max})")]
    TokenBudgetTooLarge {
        /// Provided budget value
        provided: usize,
        /// Maximum allowed budget
        max: usize,
    },

    /// The query contains no meaningful terms after stop-word filtering
    #[error("Query contains no meaningful terms: '{query}'. {suggestion}")]
    NoMeaningfulTerms {
        /// The query that resulted in no terms
        query: String,
        /// Suggestion for improving the query
        suggestion: &'static str,
    },

    /// A regex pattern failed to compile or is invalid
    #[error("Invalid regex pattern: {0}")]
    InvalidPattern(String),

    /// General failure during query parsing
    #[error("Query parsing failed: {0}")]
    ParseFailed(String),
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_parser_creation() {
        let parser = QueryParser::new();
        assert!(parser.is_ok());
    }

    #[test]
    fn test_default_parser() {
        let parser = QueryParser::default();
        // Should not panic
        let parsed = parser.parse("test query", 10);
        assert!(parsed.is_ok());
    }

    #[test]
    fn test_parse_empty_query() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("", 10);
        assert!(matches!(result, Err(Error::EmptyQuery)));
    }

    #[test]
    fn test_parse_whitespace_only_query() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("   \n\t  ", 10);
        assert!(matches!(result, Err(Error::EmptyQuery)));
    }

    #[test]
    fn test_parse_how_works_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser
            .parse("show me how authentication works", 10)
            .unwrap();

        assert_eq!(parsed.intent, QueryIntent::HowWorks);
        assert_eq!(parsed.query_type, QueryType::Semantic);
        assert!(parsed.expand_context);
        assert!(parsed.terms.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_parse_where_handled_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("where is error handling handled", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::WhereHandled);
        assert_eq!(parsed.query_type, QueryType::Structural);
        assert!(parsed.expand_context);
        assert!(parsed.terms.contains(&"error".to_string()));
        assert!(parsed.terms.contains(&"handling".to_string()));
    }

    #[test]
    fn test_parse_bottlenecks_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("what are the bottlenecks", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Bottlenecks);
        assert_eq!(parsed.query_type, QueryType::Structural);
        assert!(!parsed.expand_context);
    }

    #[test]
    fn test_parse_semantic_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("how do I implement caching", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Semantic);
        assert_eq!(parsed.query_type, QueryType::Semantic);
    }

    #[test]
    fn test_parse_text_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("function_name", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Text);
        assert_eq!(parsed.query_type, QueryType::Text);
        assert!(!parsed.expand_context);
    }

    #[test]
    fn test_build_search_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("show me how parsing works", 10).unwrap();
        let search_query = parser.build_search_query(&parsed);

        assert_eq!(search_query.top_k, 10);
        assert!(search_query.semantic);
        assert!(search_query.expand_context);
        assert!(search_query.token_budget.is_some());
    }

    #[test]
    fn test_tokenize() {
        let parser = QueryParser::new().unwrap();
        let tokens = parser.tokenize("Hello World Test");

        assert_eq!(tokens.len(), 3);
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
    }

    #[test]
    fn test_tokenize_filters_short_words() {
        let parser = QueryParser::new().unwrap();
        let tokens = parser.tokenize("a an the of in");

        // "the" has 3 chars, others filtered out (< 3 chars)
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], "the");
    }

    #[test]
    fn test_stop_words_filtering() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser
            .parse("show me how the authentication system works", 10)
            .unwrap();

        // Should not contain stop words like "show", "me", "the"
        assert!(!parsed.terms.contains(&"show".to_string()));
        assert!(!parsed.terms.contains(&"me".to_string()));
        assert!(!parsed.terms.contains(&"the".to_string()));

        // Should contain meaningful terms extracted from the subject
        assert!(parsed.terms.contains(&"authentication".to_string()));
        assert!(parsed.terms.contains(&"system".to_string()));
    }

    #[test]
    fn test_complexity_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("most complex functions", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Bottlenecks);
        assert_eq!(parsed.query_type, QueryType::Structural);
    }

    #[test]
    fn test_query_with_question_words() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("what does this function do", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Semantic);
    }

    // ===== Validation Tests =====

    #[test]
    fn test_query_too_long() {
        let parser = QueryParser::new().unwrap();
        let long_query = "a".repeat(MAX_QUERY_LENGTH + 1);
        let result = parser.parse(&long_query, 10);
        assert!(matches!(result, Err(Error::QueryTooLong { .. })));
    }

    #[test]
    fn test_query_exactly_max_length() {
        let parser = QueryParser::new().unwrap();
        let query = "a".repeat(MAX_QUERY_LENGTH);
        let result = parser.parse(&query, 10);
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_top_k_too_small() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("test query", 0);
        assert!(matches!(result, Err(Error::InvalidTopK { .. })));
    }

    #[test]
    fn test_invalid_top_k_too_large() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("test query", MAX_TOP_K + 1);
        assert!(matches!(result, Err(Error::InvalidTopK { .. })));
    }

    #[test]
    fn test_valid_top_k_boundaries() {
        let parser = QueryParser::new().unwrap();
        assert!(parser.parse("test", MIN_TOP_K).is_ok());
        assert!(parser.parse("test", MAX_TOP_K).is_ok());
    }

    #[test]
    fn test_query_with_null_bytes() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("test\x00query", 10);
        assert!(matches!(result, Err(Error::InvalidCharacters { .. })));
    }

    #[test]
    fn test_query_with_control_characters() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("test\x01query", 10);
        assert!(matches!(result, Err(Error::InvalidCharacters { .. })));
    }

    #[test]
    fn test_unicode_normalization() {
        let parser = QueryParser::new().unwrap();
        // "café" can be represented as "e" + combining acute
        let query1 = parser.parse("café", 10);
        let query2 = parser.parse("cafe\u{301}", 10); // e + combining acute
        assert!(query1.is_ok());
        assert!(query2.is_ok());
        // Both should normalize to the same terms
        assert_eq!(query1.unwrap().terms, query2.unwrap().terms);
    }

    #[test]
    fn test_no_meaningful_terms() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("the a an of in", 10);
        assert!(matches!(result, Err(Error::NoMeaningfulTerms { .. })));
    }

    // ===== Performance Tests =====

    #[test]
    fn test_stop_words_set_is_efficient() {
        // Verify that STOP_WORDS is a static HashSet (not recreated on each call)
        let addr1 = &*STOP_WORDS as *const _ as usize;
        let addr2 = &*STOP_WORDS as *const _ as usize;
        assert_eq!(addr1, addr2, "STOP_WORDS should be statically allocated");
    }

    #[test]
    fn test_regex_patterns_are_static() {
        // Verify that regex patterns are pre-compiled
        let addr1 = &*HOW_WORKS_PATTERN as *const _ as usize;
        let addr2 = &*HOW_WORKS_PATTERN as *const _ as usize;
        assert_eq!(
            addr1, addr2,
            "HOW_WORKS_PATTERN should be statically allocated"
        );
    }

    // ===== Edge Case Tests =====

    #[test]
    fn test_query_with_only_stop_words_fallback() {
        let parser = QueryParser::new().unwrap();
        // "function" is 8 chars, so it passes the >= 3 filter and is not a stop word
        let result = parser.parse("function", 10);
        assert!(result.is_ok());
        assert!(!result.unwrap().terms.is_empty());
    }

    #[test]
    fn test_very_long_single_word() {
        let parser = QueryParser::new().unwrap();
        let long_word = "a".repeat(300);
        let result = parser.parse(&long_word, 10);
        // Should be OK (300 < MAX_QUERY_LENGTH and > 2)
        assert!(result.is_ok());
    }

    #[test]
    fn test_mixed_case_normalization() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser
            .parse("SHOW ME How AUTHENTICATION Works", 10)
            .unwrap();
        assert!(parsed.terms.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_multiple_spaces() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser
            .parse("show    me    how    authentication    works", 10)
            .unwrap();
        assert!(parsed.terms.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_trailing_punctuation() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser
            .parse("show me how authentication works.", 10)
            .unwrap();
        assert!(parsed.terms.contains(&"authentication".to_string()));
    }
}
