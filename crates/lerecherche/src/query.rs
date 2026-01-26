// Natural Language Query Processing
//
// *La Question* (The Question) - Convert natural language to structured search

use crate::ranking::QueryType;
use crate::search::{SearchQuery, SearchResult};
use crate::search::Error as SearchError;
use regex::Regex;
use std::collections::HashSet;

/// Natural language query intent
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

/// Parsed natural language query
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Original query text
    pub original: String,

    /// Extracted key terms
    pub terms: Vec<String>,

    /// Query intent
    pub intent: QueryIntent,

    /// Query type for ranking
    pub query_type: QueryType,

    /// Whether to expand context
    pub expand_context: bool,

    /// Maximum results
    pub top_k: usize,

    /// Token budget for context expansion
    pub token_budget: Option<usize>,
}

/// Natural language query parser
pub struct QueryParser {
    /// Pattern for "show me how X works"
    how_works_pattern: Regex,

    /// Pattern for "where is X handled"
    where_handled_pattern: Regex,

    /// Pattern for "what are bottlenecks"
    bottlenecks_pattern: Regex,

    /// Pattern for complexity-related queries
    complexity_pattern: Regex,
}

impl QueryParser {
    /// Create a new query parser
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            how_works_pattern: Regex::new(
                r"(?i)(?:show|tell|explain|describe)\s+(?:me\s+)?how\s+(?:does\s+)?(.+?)(?:\s+(?:work|works|working|function|functions|operate|operates))?\s*\.?\s*$"
            ).map_err(|e| Error::InvalidPattern(e.to_string()))?,

            where_handled_pattern: Regex::new(
                r"(?i)where\s+(?:is|are|do\s+we\s+handle|does\s+.\s+handle)\s+(.+?)(?:\s+handled)?\s*\.?\s*$"
            ).map_err(|e| Error::InvalidPattern(e.to_string()))?,

            bottlenecks_pattern: Regex::new(
                r"(?i)(what|where|find)\s+(?:are\s+)?(?:the\s+)?(bottlenecks|performance\s+issues|slow\s+code|optimization\s+opportunities)"
            ).map_err(|e| Error::InvalidPattern(e.to_string()))?,

            complexity_pattern: Regex::new(
                r"(?i)(most|least)\s+(complex|complicated|difficult|simple)"
            ).map_err(|e| Error::InvalidPattern(e.to_string()))?,
        })
    }

    /// Parse a natural language query
    pub fn parse(&self, query: &str, default_top_k: usize) -> Result<ParsedQuery, Error> {
        let query = query.trim();

        if query.is_empty() {
            return Err(Error::EmptyQuery);
        }

        // Detect intent
        let intent = self.detect_intent(query);

        // Extract terms based on intent
        let terms = self.extract_terms(query, &intent);

        // Determine query type
        let query_type = self.classify_query(&intent);

        // Determine if we should expand context
        let expand_context = matches!(
            intent,
            QueryIntent::HowWorks | QueryIntent::WhereHandled
        );

        // Set token budget for context expansion
        let token_budget = if expand_context {
            Some(2000) // Default token budget
        } else {
            None
        };

        Ok(ParsedQuery {
            original: query.to_string(),
            terms,
            intent,
            query_type,
            expand_context,
            top_k: default_top_k,
            token_budget,
        })
    }

    /// Detect the intent of the query
    fn detect_intent(&self, query: &str) -> QueryIntent {
        let query_lower = query.to_lowercase();

        // Check for "show me how X works" pattern
        if self.how_works_pattern.is_match(query) {
            return QueryIntent::HowWorks;
        }

        // Check for "where is X handled" pattern
        if self.where_handled_pattern.is_match(query) {
            return QueryIntent::WhereHandled;
        }

        // Check for "what are bottlenecks" pattern
        if self.bottlenecks_pattern.is_match(query) {
            return QueryIntent::Bottlenecks;
        }

        // Check for complexity-related queries
        if self.complexity_pattern.is_match(query) {
            return QueryIntent::Bottlenecks; // Treat complexity queries as bottleneck queries
        }

        // Check if query contains question words (likely semantic)
        if query_lower.contains("how") || query_lower.contains("what")
            || query_lower.contains("why") || query_lower.contains("where")
            || query_lower.contains("when") || query_lower.contains("which")
        {
            return QueryIntent::Semantic;
        }

        // Default to text search
        QueryIntent::Text
    }

    /// Extract key terms from the query
    fn extract_terms(&self, query: &str, intent: &QueryIntent) -> Vec<String> {
        let mut terms = Vec::new();

        match intent {
            QueryIntent::HowWorks => {
                // Extract subject from "show me how X works"
                if let Some(captures) = self.how_works_pattern.captures(query) {
                    if let Some(subject) = captures.get(1) {
                        terms.extend(self.tokenize(subject.as_str()));
                    } else {
                        // Fallback: extract all terms from query
                        terms.extend(self.tokenize(query));
                    }
                } else {
                    terms.extend(self.tokenize(query));
                }
            }
            QueryIntent::WhereHandled => {
                // Extract subject from "where is X handled"
                if let Some(captures) = self.where_handled_pattern.captures(query) {
                    if let Some(subject) = captures.get(1) {
                        terms.extend(self.tokenize(subject.as_str()));
                    } else {
                        // Fallback: extract all terms from query
                        terms.extend(self.tokenize(query));
                    }
                } else {
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

        // Remove duplicates and filter stop words
        let stop_words = self.stop_words();
        terms = terms.into_iter()
            .filter(|t| !stop_words.contains(t.as_str()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // If no terms remain after stop word filtering, return some terms from the query
        if terms.is_empty() {
            terms = self.tokenize(query);
        }

        terms
    }

    /// Tokenize text into individual terms
    fn tokenize(&self, text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|s| s.to_lowercase())
            .filter(|s| s.len() > 2) // Filter out very short words
            .collect()
    }

    /// Get common stop words
    fn stop_words(&self) -> HashSet<String> {
        [
            "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for",
            "of", "with", "by", "from", "as", "is", "was", "are", "were", "been",
            "be", "have", "has", "had", "do", "does", "did", "will", "would",
            "could", "should", "may", "might", "must", "shall", "can", "need",
            "show", "me", "tell", "explain", "describe", "how", "what", "where",
            "when", "why", "which", "that", "this", "these", "those",
        ]
        .iter()
        .map(|s| s.to_string())
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
        }
    }
}

impl Default for QueryParser {
    fn default() -> Self {
        Self::new().expect("Failed to create QueryParser")
    }
}

/// Natural language query errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Empty query")]
    EmptyQuery,

    #[error("Invalid regex pattern: {0}")]
    InvalidPattern(String),

    #[error("Query parsing failed: {0}")]
    ParseFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_parser_creation() {
        let parser = QueryParser::new();
        assert!(parser.is_ok());
    }

    #[test]
    fn test_parse_empty_query() {
        let parser = QueryParser::new().unwrap();
        let result = parser.parse("", 10);
        assert!(matches!(result, Err(Error::EmptyQuery)));
    }

    #[test]
    fn test_parse_how_works_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("show me how authentication works", 10).unwrap();

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
    fn test_stop_words_filtering() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("show me how the authentication system works", 10).unwrap();

        // Should not contain stop words like "show", "me", "the"
        assert!(!parsed.terms.contains(&"show".to_string()));
        assert!(!parsed.terms.contains(&"me".to_string()));
        assert!(!parsed.terms.contains(&"the".to_string()));

        // Should contain meaningful terms extracted from the subject
        // The regex captures "authentication system" (without "works" which is part of the pattern)
        assert!(parsed.terms.contains(&"authentication".to_string()));
        assert!(parsed.terms.contains(&"system".to_string()));
    }

    #[test]
    fn test_complexity_query() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("find the most complex functions", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Bottlenecks);
        assert_eq!(parsed.query_type, QueryType::Structural);
    }

    #[test]
    fn test_query_with_question_words() {
        let parser = QueryParser::new().unwrap();
        let parsed = parser.parse("what does this function do", 10).unwrap();

        assert_eq!(parsed.intent, QueryIntent::Semantic);
    }
}
