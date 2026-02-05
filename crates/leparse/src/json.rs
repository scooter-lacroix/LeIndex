// JSON data format parser implementation

use crate::traits::{
    Block, CodeIntelligence, ComplexityMetrics, Edge, Error, Graph, Result, SignatureInfo,
};

/// JSON parser with full CodeIntelligence implementation
pub struct JsonParser;

impl Default for JsonParser {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonParser {
    /// Create a new JSON parser
    pub fn new() -> Self {
        Self
    }
}

impl CodeIntelligence for JsonParser {
    fn get_signatures(&self, _source: &[u8]) -> Result<Vec<SignatureInfo>> {
        // JSON doesn't really have signatures, but we could treat keys as such.
        // For now, return empty vector or treat whole file as a signature.
        Ok(vec![])
    }

    fn get_signatures_with_parser(
        &self,
        source: &[u8],
        parser: &mut tree_sitter::Parser,
    ) -> Result<Vec<SignatureInfo>> {
        parser
            .set_language(&crate::traits::languages::json::language())
            .map_err(|e| Error::ParseFailed(e.to_string()))?;

        let _tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::ParseFailed("Failed to parse JSON source".to_string()))?;

        Ok(vec![])
    }

    fn compute_cfg(&self, _source: &[u8], _node_id: usize) -> Result<Graph<Block, Edge>> {
        Ok(Graph {
            blocks: vec![],
            edges: vec![],
            entry_block: 0,
            exit_blocks: vec![],
        })
    }

    fn extract_complexity(&self, _node: &tree_sitter::Node) -> ComplexityMetrics {
        ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: 0,
            token_count: 0,
        }
    }
}
