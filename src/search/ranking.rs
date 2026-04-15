// Hybrid scoring algorithm

use serde::{Deserialize, Serialize};

/// Combined score from multiple signals
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct Score {
    /// Overall score (0-1)
    pub overall: f32,

    /// Semantic similarity component
    pub semantic: f32,

    /// Structural relevance component
    pub structural: f32,

    /// Text match component
    pub text_match: f32,
}

impl Score {
    /// Create a new score
    pub fn new(semantic: f32, structural: f32, text_match: f32) -> Self {
        // Combine with weighted average
        let overall = semantic * 0.5 + structural * 0.3 + text_match * 0.2;
        Self {
            overall,
            semantic,
            structural,
            text_match,
        }
    }

    /// Get the overall score
    pub fn value(&self) -> f32 {
        self.overall
    }
}

/// Hybrid scorer combining semantic and structural signals
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HybridScorer {
    /// Weight for semantic component
    semantic_weight: f32,

    /// Weight for structural component
    structural_weight: f32,

    /// Weight for text match component
    text_weight: f32,
}

impl HybridScorer {
    /// Create a new hybrid scorer
    ///
    /// Default weights are optimized for code search:
    /// - semantic: 0.25 (de-weighted embeddings)
    /// - structural: 0.15 (moderate complexity signal)
    /// - text: 0.60 (strong keyword matching)
    pub fn new() -> Self {
        Self {
            semantic_weight: 0.25,
            structural_weight: 0.15,
            text_weight: 0.60,
        }
    }

    /// Scorer tuned for code symbol search (text-dominant)
    ///
    /// Optimized for finding code symbols where exact name matching
    /// and keyword overlap are most important.
    pub fn for_code() -> Self {
        Self {
            semantic_weight: 0.25,
            structural_weight: 0.15,
            text_weight: 0.60,
        }
    }

    /// Scorer tuned for natural-language/prose search
    ///
    /// Optimized for searching documentation, READMEs, and other
    /// prose where semantic understanding is more valuable.
    pub fn for_prose() -> Self {
        Self {
            semantic_weight: 0.50,
            structural_weight: 0.10,
            text_weight: 0.40,
        }
    }

    /// Set custom weights
    pub fn with_weights(mut self, semantic: f32, structural: f32, text: f32) -> Self {
        self.semantic_weight = semantic;
        self.structural_weight = structural;
        self.text_weight = text;
        self
    }

    /// Calculate combined score
    pub fn score(&self, semantic: f32, structural: f32, text_match: f32) -> Score {
        let overall = semantic * self.semantic_weight
            + structural * self.structural_weight
            + text_match * self.text_weight;

        Score {
            overall: overall.clamp(0.0, 1.0),
            semantic,
            structural,
            text_match,
        }
    }

    /// Re-rank results based on query type
    pub fn rerank(&self, results: Vec<ScoreResult>, query_type: QueryType) -> Vec<ScoreResult> {
        let mut ranked = results;
        match query_type {
            QueryType::Semantic => {
                // Boost semantic scores
                for result in &mut ranked {
                    result.score.semantic *= 1.2;
                    result.score.overall = result.score.semantic * self.semantic_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
            QueryType::Structural => {
                // Boost structural scores
                for result in &mut ranked {
                    result.score.structural *= 1.2;
                    result.score.overall = result.score.semantic * self.semantic_weight
                        + result.score.structural * self.structural_weight
                        + result.score.text_match * self.text_weight;
                }
            }
            QueryType::Text => {
                // Boost text match scores
                for result in &mut ranked {
                    result.score.text_match *= 1.2;
                    result.score.overall = result.score.semantic * self.semantic_weight
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
    /// Semantic-heavy query
    Semantic,

    /// Structural-heavy query
    Structural,

    /// Text-heavy query
    Text,
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
    fn test_score_creation() {
        let score = Score::new(0.9, 0.7, 0.5);
        assert_eq!(score.semantic, 0.9);
        assert_eq!(score.structural, 0.7);
        assert_eq!(score.text_match, 0.5);
    }

    #[test]
    fn test_hybrid_scorer() {
        let scorer = HybridScorer::new();
        let score = scorer.score(0.8, 0.6, 0.4);
        // New default weights: 0.25 * 0.8 + 0.15 * 0.6 + 0.60 * 0.4 = 0.53
        assert!((score.overall - 0.53).abs() < 0.01);
    }

    #[test]
    fn test_custom_weights() {
        let scorer = HybridScorer::new().with_weights(0.3, 0.5, 0.2);
        let score = scorer.score(0.8, 0.6, 0.4);
        // Custom weights: 0.3 * 0.8 + 0.5 * 0.6 + 0.2 * 0.4 = 0.62
        assert!((score.overall - 0.62).abs() < 0.01);
    }

    #[test]
    fn test_for_code_scorer() {
        let scorer = HybridScorer::for_code();
        let score = scorer.score(0.8, 0.6, 0.4);
        // Code weights: 0.25 * 0.8 + 0.15 * 0.6 + 0.60 * 0.4 = 0.53
        assert!((score.overall - 0.53).abs() < 0.01);
    }

    #[test]
    fn test_for_prose_scorer() {
        let scorer = HybridScorer::for_prose();
        let score = scorer.score(0.8, 0.6, 0.4);
        // Prose weights: 0.50 * 0.8 + 0.10 * 0.6 + 0.40 * 0.4 = 0.62
        assert!((score.overall - 0.62).abs() < 0.01);
    }
}
