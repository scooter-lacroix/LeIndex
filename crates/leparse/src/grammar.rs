// Lazy-loaded grammar cache for memory-efficient parsing
//
// This module provides a unified language registry that combines:
// - Grammar caching (via GrammarCache)
// - Language configuration (via LanguageConfig in traits.rs)
// - Lazy-loaded grammar loading

use once_cell::sync::Lazy;
use std::sync::RwLock;
use tree_sitter::Language;

/// Grammar cache entry
#[derive(Debug, Clone)]
struct GrammarCacheEntry {
    /// The tree-sitter language
    language: Language,
}

/// Thread-safe grammar cache
///
/// This cache stores tree-sitter Language objects in a lazy-loaded manner,
/// ensuring that grammars are only loaded when first accessed and then
/// reused for subsequent parsing operations.
#[derive(Debug, Default)]
pub struct GrammarCache {
    /// Internal storage for cached grammars
    /// Using RwLock for thread-safe read-write access
    grammars: RwLock<Vec<Option<GrammarCacheEntry>>>,
}

impl GrammarCache {
    /// Create a new empty grammar cache
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a language by index, loading it lazily if needed
    ///
    /// # Arguments
    /// * `index` - Grammar index (corresponds to language IDs)
    /// * `loader` - Function to load the grammar if not cached
    ///
    /// # Returns
    /// The tree-sitter Language for the given index
    pub fn get_or_load<F>(&self, index: usize, loader: F) -> Result<Language, crate::traits::Error>
    where
        F: FnOnce() -> Language,
    {
        // Try to read from cache first (optimistic read path)
        {
            let read_guard = self.grammars
                .read()
                .map_err(|e| crate::traits::Error::ParseFailed(format!("Cache lock poisoned: {}", e)))?;

            // Ensure the vector is large enough
            if read_guard.len() > index {
                if let Some(entry) = &read_guard[index] {
                    return Ok(entry.language.clone());
                }
            }
        }

        // Need to load the grammar (write path)
        let mut write_guard = self.grammars
            .write()
            .map_err(|e| crate::traits::Error::ParseFailed(format!("Cache lock poisoned: {}", e)))?;

        // Double-check: another thread might have loaded it while we waited
        if write_guard.len() > index {
            if let Some(entry) = &write_guard[index] {
                return Ok(entry.language.clone());
            }
        }

        // Ensure the vector is large enough
        while write_guard.len() <= index {
            write_guard.push(None);
        }

        // Load and cache the grammar
        let language = loader();
        let entry = GrammarCacheEntry { language: language.clone() };
        write_guard[index] = Some(entry);

        Ok(language)
    }

    /// Get the number of cached grammars
    pub fn len(&self) -> usize {
        self.grammars
            .read()
            .map(|g| g.len())
            .unwrap_or(0)
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Global grammar cache instance
///
/// This is a lazy-static global cache that is shared across all parsing operations.
/// Grammars are loaded on first use and cached for the lifetime of the program.
pub static GLOBAL_GRAMMAR_CACHE: Lazy<GrammarCache> = Lazy::new(GrammarCache::new);

/// Unified language registry
///
/// This enum provides a single source of truth for language identification,
/// combining the previous LanguageId with LanguageConfig integration.
/// The discriminants (0, 1, 2, 3, 4) correspond to cache indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Python = 0,
    JavaScript = 1,
    TypeScript = 2,
    Go = 3,
    Rust = 4,
}

impl LanguageId {
    /// Get the LanguageId for a file extension
    ///
    /// This is the unified entry point for language detection.
    /// Delegates to LanguageConfig::from_extension for consistency.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "py" => Some(LanguageId::Python),
            "js" | "jsx" => Some(LanguageId::JavaScript),
            "ts" | "tsx" => Some(LanguageId::TypeScript),
            "go" => Some(LanguageId::Go),
            "rs" => Some(LanguageId::Rust),
            _ => None,
        }
    }

    /// Get the LanguageConfig for this language
    ///
    /// Provides access to the full language configuration including
    /// extensions and query patterns.
    pub fn config(&self) -> &'static crate::traits::LanguageConfig {
        match self {
            LanguageId::Python => &crate::traits::languages::python::CONFIG,
            LanguageId::JavaScript => &crate::traits::languages::javascript::CONFIG,
            LanguageId::TypeScript => &crate::traits::languages::typescript::CONFIG,
            LanguageId::Go => &crate::traits::languages::go::CONFIG,
            LanguageId::Rust => &crate::traits::languages::rust::CONFIG,
        }
    }

    /// Load the tree-sitter Language for this LanguageId
    ///
    /// Uses the centralized language loading functions from traits::languages
    /// to avoid duplicate unsafe FFI declarations.
    fn load_language(&self) -> Language {
        match self {
            LanguageId::Python => crate::traits::languages::python::language(),
            LanguageId::JavaScript => crate::traits::languages::javascript::language(),
            LanguageId::TypeScript => crate::traits::languages::typescript::language(),
            LanguageId::Go => crate::traits::languages::go::language(),
            LanguageId::Rust => crate::traits::languages::rust::language(),
        }
    }

    /// Get the language from the global cache (lazy-loaded)
    ///
    /// This is the primary method for obtaining a Language object.
    /// It uses lazy loading via the global cache.
    pub fn from_cache(&self) -> Result<Language, crate::traits::Error> {
        GLOBAL_GRAMMAR_CACHE.get_or_load(*self as usize, || self.load_language())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grammar_cache_creation() {
        let cache = GrammarCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_grammar_cache_lazy_loading() {
        let cache = GrammarCache::new();

        // Load grammar for Python (index 0)
        let lang = cache.get_or_load(0, || crate::traits::languages::python::language());
        assert!(lang.is_ok());
        assert_eq!(cache.len(), 1);

        // Get the same grammar again (should be cached)
        let lang2 = cache.get_or_load(0, || {
            panic!("Should not call loader for cached grammar");
        });
        assert!(lang2.is_ok());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_language_id_from_extension() {
        assert_eq!(LanguageId::from_extension("py"), Some(LanguageId::Python));
        assert_eq!(LanguageId::from_extension("js"), Some(LanguageId::JavaScript));
        assert_eq!(LanguageId::from_extension("jsx"), Some(LanguageId::JavaScript));
        assert_eq!(LanguageId::from_extension("rs"), Some(LanguageId::Rust));
        assert_eq!(LanguageId::from_extension("unknown"), None);
    }

    #[test]
    fn test_language_id_case_insensitive() {
        assert_eq!(LanguageId::from_extension("PY"), Some(LanguageId::Python));
        assert_eq!(LanguageId::from_extension("Rs"), Some(LanguageId::Rust));
    }

    #[test]
    fn test_language_config_integration() {
        // Test that LanguageId.config() returns the correct LanguageConfig
        let py_config = LanguageId::Python.config();
        assert_eq!(py_config.name, "Python");
        assert!(py_config.extensions.contains(&"py".to_string()));

        let js_config = LanguageId::JavaScript.config();
        assert_eq!(js_config.name, "JavaScript");
        assert!(js_config.extensions.contains(&"js".to_string()));
    }

    #[test]
    fn test_unified_language_loading() {
        // Test that LanguageId.from_cache() works correctly
        let lang = LanguageId::Python.from_cache();
        assert!(lang.is_ok());

        // Load again - should be cached
        let lang2 = LanguageId::Python.from_cache();
        assert!(lang2.is_ok());
    }
}
