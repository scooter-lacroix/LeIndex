// Hybrid scoring algorithm

use serde::{Deserialize, Serialize};

/// Combined score from multiple signals
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct Score {
    /// Overall score (0-1)
    pub overall: f32,

    /// TF-IDF similarity component (keyword-based semantic)
    pub tfidf: f32,

    /// Neural/remote similarity component (deep semantic)
    pub neural: f32,

    /// Structural relevance component
    pub structural: f32,

    /// Text match component
    pub text_match: f32,
}

impl Score {
    /// Create a new score using default code-search weights (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use new_hybrid instead for TF-IDF + neural scoring"
    )]
    pub fn new(semantic: f32, structural: f32, text_match: f32) -> Self {
        Self::new_hybrid(semantic, 0.0, structural, text_match)
    }

    /// Create a new hybrid score with TF-IDF and neural components
    pub fn new_hybrid(tfidf: f32, neural: f32, structural: f32, text_match: f32) -> Self {
        let overall = HybridScorer::new()
            .score_hybrid(tfidf, neural, structural, text_match)
            .overall;
        Self {
            overall,
            tfidf,
            neural,
            structural,
            text_match,
        }
    }

    /// Get the overall score
    pub fn value(&self) -> f32 {
        self.overall
    }
}

/// Hybrid scorer combining TF-IDF, neural, structural, and text signals
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HybridScorer {
    /// Weight for TF-IDF component
    tfidf_weight: f32,

    /// Weight for neural component
    neural_weight: f32,

    /// Weight for structural component
    structural_weight: f32,

    /// Weight for text match component
    text_weight: f32,
}

impl HybridScorer {
    /// Create a new hybrid scorer
    ///
    /// Default weights are optimized for code search with neural embeddings:
    /// - tfidf: 0.30 (keyword-based semantic)
    /// - neural: 0.40 (deep semantic understanding)
    /// - structural: 0.15 (moderate complexity signal)
    /// - text: 0.15 (exact keyword matching)
    pub fn new() -> Self {
        Self::for_code()
    }

    /// Scorer tuned for code symbol search (with neural embeddings)
    ///
    /// Optimized for finding code symbols where semantic understanding
    /// and keyword overlap are both important.
    pub fn for_code() -> Self {
        Self {
            tfidf_weight: 0.30,
            neural_weight: 0.40,
            structural_weight: 0.15,
            text_weight: 0.15,
        }
    }

    /// Scorer tuned for code search without neural embeddings (TF-IDF only)
    ///
    /// When neural embeddings are unavailable, TF-IDF gets higher weight.
    pub fn for_code_without_neural() -> Self {
        Self {
            tfidf_weight: 0.60,
            neural_weight: 0.00,
            structural_weight: 0.20,
            text_weight: 0.20,
        }
    }

    /// Scorer tuned for natural-language/prose search
    ///
    /// Optimized for searching documentation, READMEs, and other
    /// prose where semantic understanding is more valuable.
    pub fn for_prose() -> Self {
        Self {
            tfidf_weight: 0.25,
            neural_weight: 0.55,
            structural_weight: 0.10,
            text_weight: 0.10,
        }
    }

    /// Set custom weights (legacy method for compatibility)
    #[deprecated(since = "1.6.4", note = "Use with_weights_hybrid instead")]
    pub fn with_weights(mut self, semantic: f32, structural: f32, text: f32) -> Self {
        // Map legacy semantic to tfidf for backward compatibility
        self.tfidf_weight = semantic;
        self.neural_weight = 0.0;
        self.structural_weight = structural;
        self.text_weight = text;
        self
    }

    /// Set custom hybrid weights
    pub fn with_weights_hybrid(
        mut self,
        tfidf: f32,
        neural: f32,
        structural: f32,
        text: f32,
    ) -> Self {
        self.tfidf_weight = tfidf;
        self.neural_weight = neural;
        self.structural_weight = structural;
        self.text_weight = text;
        self
    }

    /// Calculate combined score (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use score_hybrid instead for TF-IDF + neural scoring"
    )]
    pub fn score(&self, semantic: f32, structural: f32, text_match: f32) -> Score {
        self.score_hybrid(semantic, 0.0, structural, text_match)
    }

    /// Calculate combined hybrid score with TF-IDF and neural components
    pub fn score_hybrid(&self, tfidf: f32, neural: f32, structural: f32, text_match: f32) -> Score {
        let overall = tfidf * self.tfidf_weight
            + neural * self.neural_weight
            + structural * self.structural_weight
            + text_match * self.text_weight;

        Score {
            overall: overall.clamp(0.0, 1.0),
            tfidf,
            neural,
            structural,
            text_match,
        }
    }

    /// Re-rank results based on query type (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use rerank_hybrid instead for TF-IDF + neural reranking"
    )]
    pub fn rerank(&self, results: Vec<ScoreResult>, query_type: QueryType) -> Vec<ScoreResult> {
        self.rerank_hybrid(results, query_type)
    }

    /// Re-rank hybrid results based on query type
    pub fn rerank_hybrid(
        &self,
        results: Vec<ScoreResult>,
        query_type: QueryType,
    ) -> Vec<ScoreResult> {
        let mut ranked = results;
        match query_type {
            QueryType::Semantic => {
                // Boost neural and TF-IDF scores
                for result in &mut ranked {
                    result.score.neural *= 1.2;
                    result.score.tfidf *= 1.1;
                    result.score.overall = result.score.tfidf * self.tfidf_weight
                        + result.score.neural * self.neural_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
            QueryType::Structural => {
                // Boost structural scores
                for result in &mut ranked {
                    result.score.structural *= 1.2;
                    result.score.overall = result.score.tfidf * self.tfidf_weight
                        + result.score.neural * self.neural_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
            QueryType::Text => {
                // Boost text match scores
                for result in &mut ranked {
                    result.score.text_match *= 1.2;
                    result.score.overall = result.score.tfidf * self.tfidf_weight
                        + result.score.neural * self.neural_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
            QueryType::Exact => {
                // Boost text match scores even more aggressively for exact mode
                for result in &mut ranked {
                    result.score.text_match *= 1.5;
                    result.score.overall = result.score.tfidf * self.tfidf_weight
                        + result.score.neural * self.neural_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
        }

        ranked.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        ranked
    }
}

/// Query type for adaptive ranking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryType {
    /// Semantic-heavy query (conceptual relevance, TF-IDF focused)
    Semantic,

    /// Structural-heavy query
    Structural,

    /// Text-heavy query
    Text,

    /// Exact-match query (prioritize exact symbol name matches)
    Exact,
}

/// Score result with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    /// Node ID
    pub node_id: String,

    /// Calculated score
    pub score: Score,

    /// Query type detected
    pub query_type: QueryType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_creation_legacy() {
        #[allow(deprecated)]
        let score = Score::new(0.9, 0.7, 0.5);
        assert_eq!(score.tfidf, 0.9); // Legacy semantic mapped to tfidf
        assert_eq!(score.neural, 0.0);
        assert_eq!(score.structural, 0.7);
        assert_eq!(score.text_match, 0.5);
    }

    #[test]
    fn test_score_creation_hybrid() {
        let score = Score::new_hybrid(0.7, 0.9, 0.6, 0.5);
        assert_eq!(score.tfidf, 0.7);
        assert_eq!(score.neural, 0.9);
        assert_eq!(score.structural, 0.6);
        assert_eq!(score.text_match, 0.5);
    }

    #[test]
    fn test_hybrid_scorer_legacy() {
        #[allow(deprecated)]
        {
            let scorer = HybridScorer::new();
            let score = scorer.score(0.8, 0.6, 0.4);
            // Default weights: 0.30 * 0.8 + 0.40 * 0.0 + 0.15 * 0.6 + 0.15 * 0.4 = 0.39
            assert!((score.overall - 0.39).abs() < 0.01);
        }
    }

    #[test]
    fn test_hybrid_scorer_new() {
        let scorer = HybridScorer::new();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Default weights: 0.30 * 0.8 + 0.40 * 0.9 + 0.15 * 0.6 + 0.15 * 0.4 = 0.75
        assert!((score.overall - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_custom_weights_legacy() {
        #[allow(deprecated)]
        let scorer = HybridScorer::new().with_weights(0.3, 0.5, 0.2);
        #[allow(deprecated)]
        let score = scorer.score(0.8, 0.6, 0.4);
        // Custom weights (mapped): 0.3 * 0.8 + 0.0 * 0.0 + 0.5 * 0.6 + 0.2 * 0.4 = 0.62
        assert!((score.overall - 0.62).abs() < 0.01);
    }

    #[test]
    fn test_custom_weights_hybrid() {
        let scorer = HybridScorer::new().with_weights_hybrid(0.3, 0.4, 0.2, 0.1);
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Custom hybrid weights: 0.3 * 0.8 + 0.4 * 0.9 + 0.2 * 0.6 + 0.1 * 0.4 = 0.76
        assert!((score.overall - 0.76).abs() < 0.01);
    }

    #[test]
    fn test_for_code_scorer_with_neural() {
        let scorer = HybridScorer::for_code();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Code weights with neural: 0.30 * 0.8 + 0.40 * 0.9 + 0.15 * 0.6 + 0.15 * 0.4 = 0.75
        assert!((score.overall - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_for_code_scorer_without_neural() {
        let scorer = HybridScorer::for_code_without_neural();
        let score = scorer.score_hybrid(0.8, 0.0, 0.6, 0.4);
        // Code weights without neural: 0.60 * 0.8 + 0.00 * 0.0 + 0.20 * 0.6 + 0.20 * 0.4 = 0.68
        assert!((score.overall - 0.68).abs() < 0.01);
    }

    #[test]
    fn test_for_prose_scorer() {
        let scorer = HybridScorer::for_prose();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Prose weights: 0.25 * 0.8 + 0.55 * 0.9 + 0.10 * 0.6 + 0.10 * 0.4 = 0.795
        assert!((score.overall - 0.795).abs() < 0.01);
    }
}
