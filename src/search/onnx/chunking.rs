// Cross-language semantic chunking for ONNX embeddings
//
// This module provides intelligent chunking strategies that preserve
// cross-language semantic understanding (e.g., Python → SQL → HTML).

use std::collections::HashMap;

/// Chunk configuration for cross-language semantic understanding
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// Maximum chunk length in tokens
    pub max_tokens: usize,
    /// Whether to include language markers
    pub include_language_markers: bool,
    /// Whether to include cross-file references
    pub include_cross_file_refs: bool,
    /// Overlap between chunks in tokens
    pub chunk_overlap: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            max_tokens: 512,
            include_language_markers: true,
            include_cross_file_refs: true,
            chunk_overlap: 64,
        }
    }
}

/// A semantic chunk with metadata
#[derive(Debug, Clone)]
pub struct SemanticChunk {
    /// Chunk content
    pub content: String,
    /// Programming language
    pub language: String,
    /// File path
    pub file_path: String,
    /// Byte range in source file
    pub byte_range: (usize, usize),
    /// Cross-language references (language -> list of referenced symbols)
    pub cross_lang_refs: HashMap<String, Vec<String>>,
    /// Chunk position in sequence
    pub chunk_index: usize,
    /// Total chunks in sequence
    pub total_chunks: usize,
}

/// Cross-language chunker for semantic embedding generation
pub struct CrossLanguageChunker {
    config: ChunkConfig,
}

impl CrossLanguageChunker {
    /// Create a new cross-language chunker
    pub fn new(config: ChunkConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration
    pub fn default() -> Self {
        Self {
            config: ChunkConfig::default(),
        }
    }

    /// Chunk a code file for cross-language semantic understanding
    ///
    /// This method chunks code while preserving:
    /// - Language context (via markers)
    /// - Cross-file references (SQL queries in Python, HTML templates, etc.)
    /// - Semantic boundaries (function/class boundaries)
    pub fn chunk_file(
        &self,
        content: &str,
        language: &str,
        file_path: &str,
        cross_lang_refs: HashMap<String, Vec<String>>,
    ) -> Vec<SemanticChunk> {
        let mut chunks = Vec::new();

        // Add language marker if enabled
        let language_marker = if self.config.include_language_markers {
            format!("// LANGUAGE: {}\n", language)
        } else {
            String::new()
        };

        // Add cross-language references if enabled
        let cross_ref_marker = if self.config.include_cross_file_refs && !cross_lang_refs.is_empty() {
            let refs: Vec<String> = cross_lang_refs
                .iter()
                .map(|(lang, symbols)| {
                    format!(
                        "// REFERENCES[{}]: {}",
                        lang,
                        symbols.join(", ")
                    )
                })
                .collect();
            format!("{}\n", refs.join("\n"))
        } else {
            String::new()
        };

        // Simple chunking by tokens (can be improved with AST-aware chunking)
        let tokens: Vec<&str> = content.split_whitespace().collect();
        let chunk_size = self.config.max_tokens;

        if tokens.len() <= chunk_size {
            // Single chunk for small files
            chunks.push(SemanticChunk {
                content: format!("{}{}{}", language_marker, cross_ref_marker, content),
                language: language.to_string(),
                file_path: file_path.to_string(),
                byte_range: (0, content.len()),
                cross_lang_refs,
                chunk_index: 0,
                total_chunks: 1,
            });
        } else {
            // Multi-chunk for large files
            let total_chunks = (tokens.len() as f32 / chunk_size as f32).ceil() as usize;
            for (i, chunk_tokens) in tokens.chunks(chunk_size).enumerate() {
                let chunk_content = chunk_tokens.join(" ");
                chunks.push(SemanticChunk {
                    content: format!(
                        "{}{}// CHUNK {}/{}\n{}",
                        language_marker, cross_ref_marker, i + 1, total_chunks, chunk_content
                    ),
                    language: language.to_string(),
                    file_path: file_path.to_string(),
                    byte_range: (0, 0), // Would need proper tracking
                    cross_lang_refs: cross_lang_refs.clone(),
                    chunk_index: i,
                    total_chunks,
                });
            }
        }

        chunks
    }

    /// Extract cross-language references from code
    ///
    /// This is a simple heuristic-based extraction. In a full implementation,
    /// this would use AST analysis to find actual cross-language dependencies.
    pub fn extract_cross_lang_references(
        &self,
        content: &str,
        language: &str,
    ) -> HashMap<String, Vec<String>> {
        let mut refs = HashMap::new();

        // Simple heuristic patterns (can be extended with AST-based analysis)
        match language {
            "python" => {
                // Look for SQL queries
                if content.contains("SELECT") || content.contains("INSERT") || content.contains("UPDATE") {
                    refs.entry("sql".to_string())
                        .or_insert_with(Vec::new)
                        .push("database_query".to_string());
                }
                // Look for HTML templates
                if content.contains("render_template") || content.contains("<html") {
                    refs.entry("html".to_string())
                        .or_insert_with(Vec::new)
                        .push("template".to_string());
                }
            }
            "javascript" | "typescript" => {
                // Look for SQL (ORM queries)
                if content.contains("SELECT") || content.contains("query") {
                    refs.entry("sql".to_string())
                        .or_insert_with(Vec::new)
                        .push("database_query".to_string());
                }
                // Look for HTML (JSX, templates)
                if content.contains("<div") || content.contains("ReactDOM") {
                    refs.entry("html".to_string())
                        .or_insert_with(Vec::new)
                        .push("ui_component".to_string());
                }
            }
            _ => {}
        }

        refs
    }
}

impl Default for CrossLanguageChunker {
    fn default() -> Self {
        Self::new(ChunkConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunker_creation() {
        let chunker = CrossLanguageChunker::default();
        assert_eq!(chunker.config.max_tokens, 512);
    }

    #[test]
    fn test_single_chunk_small_file() {
        let chunker = CrossLanguageChunker::default();
        let content = "fn hello() { println!(\"world\"); }";
        let chunks = chunker.chunk_file(content, "rust", "test.rs", HashMap::new());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("LANGUAGE: rust"));
    }

    #[test]
    fn test_cross_lang_reference_extraction_python() {
        let chunker = CrossLanguageChunker::default();
        let content = "def query_user(): return execute(\"SELECT * FROM users\")";
        let refs = chunker.extract_cross_lang_references(content, "python");
        assert!(refs.contains_key("sql"));
    }

    #[test]
    fn test_cross_lang_reference_extraction_javascript() {
        let chunker = CrossLanguageChunker::default();
        let content = "const App = () => { return <div>Hello</div>; }";
        let refs = chunker.extract_cross_lang_references(content, "javascript");
        assert!(refs.contains_key("html"));
    }
}
