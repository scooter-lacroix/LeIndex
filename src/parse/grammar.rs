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
    pub fn get_or_load<F>(
        &self,
        index: usize,
        loader: F,
    ) -> Result<Language, crate::parse::traits::Error>
    where
        F: FnOnce() -> Language,
    {
        // Try to read from cache first (optimistic read path)
        {
            let read_guard = self.grammars.read().map_err(|e| {
                crate::parse::traits::Error::ParseFailed(format!("Cache lock poisoned: {}", e))
            })?;

            // Ensure the vector is large enough
            if read_guard.len() > index {
                if let Some(entry) = &read_guard[index] {
                    return Ok(entry.language.clone());
                }
            }
        }

        // Need to load the grammar (write path)
        let mut write_guard = self.grammars.write().map_err(|e| {
            crate::parse::traits::Error::ParseFailed(format!("Cache lock poisoned: {}", e))
        })?;

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
        let entry = GrammarCacheEntry {
            language: language.clone(),
        };
        write_guard[index] = Some(entry);

        Ok(language)
    }

    /// Get the number of cached grammars
    ///
    /// Returns 0 if the cache lock is poisoned (indicating a serious bug).
    /// The poisoning error is logged via expect() for debugging purposes.
    pub fn len(&self) -> usize {
        self.grammars
            .read()
            .map(|g| g.len())
            .unwrap_or_else(|e| {
                // Use expect with context to make debugging easier
                // This will panic with the poisoning error, which is appropriate
                // for a RwLock poisoning (indicates a serious bug)
                panic!("Grammar cache lock poisoned: {}. This indicates a serious bug in concurrent access.", e)
            })
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
/// The discriminants correspond to cache indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    /// Python programming language
    Python = 0,
    /// JavaScript programming language
    JavaScript = 1,
    /// TypeScript programming language
    TypeScript = 2,
    /// Go programming language
    Go = 3,
    /// Rust programming language
    Rust = 4,
    /// Java programming language
    Java = 5,
    /// C++ programming language
    Cpp = 6,
    /// C# programming language
    CSharp = 7,
    /// Ruby programming language
    Ruby = 8,
    /// PHP programming language
    Php = 9,
    // Lua=10..Json=14 were renumbered in v1.6.6 when Swift/Kotlin/Dart were
    // disabled; the Tier-0 breadth wave (2026-08-20) appends new variants
    // from 15 upward without renumbering existing discriminants.
    /// Lua programming language
    Lua = 10,
    /// Scala programming language
    Scala = 11,
    /// C programming language
    C = 12,
    /// Bash programming language
    Bash = 13,
    /// JSON data format
    Json = 14,
    /// Swift programming language (Tier-0 breadth wave)
    Swift = 15,
    /// Kotlin programming language (Tier-0 breadth wave)
    Kotlin = 16,
    /// Dart programming language (Tier-0 breadth wave)
    Dart = 17,
    /// HTML markup (Tier-0)
    Html = 18,
    /// CSS (Tier-0)
    Css = 19,
    /// SCSS (Tier-0)
    Scss = 20,
    /// YAML (Tier-0)
    Yaml = 21,
    /// CMake (Tier-0)
    Cmake = 22,
    /// Elixir (Tier-0)
    Elixir = 24,
    /// Erlang (Tier-0)
    Erlang = 25,
    /// Haskell (Tier-0)
    Haskell = 26,
    /// Perl (Tier-0)
    Perl = 27,
    /// R (Tier-0)
    R = 28,
    /// Zig (Tier-0)
    Zig = 29,
    /// GraphQL (Tier-0)
    Graphql = 30,
    /// HCL / Terraform (Tier-0)
    Hcl = 31,
    /// Makefile (Tier-0)
    Make = 32,
    /// Emacs Lisp (Tier-0)
    Elisp = 33,
    /// Julia (Tier-0)
    Julia = 34,
    /// D programming language (Tier-0)
    D = 35,
    /// GLSL shaders (Tier-0)
    Glsl = 36,
    /// Embedded templates (EJS/ERB/Liquid) (Tier-0)
    EmbeddedTemplate = 37,
}

impl LanguageId {
    /// Get the LanguageId for a file extension
    ///
    /// This is the unified entry point for language detection.
    /// Delegates to LanguageConfig::from_extension for consistency.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "py" => Some(LanguageId::Python),
            "js" | "mjs" | "cjs" | "jsx" => Some(LanguageId::JavaScript),
            "ts" | "tsx" | "mts" | "cts" => Some(LanguageId::TypeScript),
            "go" => Some(LanguageId::Go),
            "rs" => Some(LanguageId::Rust),
            "java" => Some(LanguageId::Java),
            "cpp" | "cc" | "cxx" | "hpp" => Some(LanguageId::Cpp),
            "h" => Some(LanguageId::C), // Default .h to C (Cpp override handles .hpp)
            "c" => Some(LanguageId::C),
            "cs" => Some(LanguageId::CSharp),
            "rb" => Some(LanguageId::Ruby),
            "php" => Some(LanguageId::Php),
            "swift" => Some(LanguageId::Swift),
            "kt" | "kts" => Some(LanguageId::Kotlin),
            "dart" => Some(LanguageId::Dart),
            "html" | "htm" => Some(LanguageId::Html),
            "css" => Some(LanguageId::Css),
            "scss" => Some(LanguageId::Scss),
            "yaml" | "yml" => Some(LanguageId::Yaml),
            "cmake" => Some(LanguageId::Cmake),
            "ex" | "exs" => Some(LanguageId::Elixir),
            "erl" | "hrl" => Some(LanguageId::Erlang),
            "hs" => Some(LanguageId::Haskell),
            "pl" | "pm" => Some(LanguageId::Perl),
            "r" => Some(LanguageId::R),
            "zig" => Some(LanguageId::Zig),
            "graphql" | "gql" => Some(LanguageId::Graphql),
            "hcl" | "tf" | "tfvars" => Some(LanguageId::Hcl),
            "makefile" | "mak" | "mk" => Some(LanguageId::Make),
            "el" => Some(LanguageId::Elisp),
            "jl" => Some(LanguageId::Julia),
            "d" | "di" => Some(LanguageId::D),
            "glsl" | "vert" | "frag" | "comp" => Some(LanguageId::Glsl),
            "ejs" | "erb" | "liquid" => Some(LanguageId::EmbeddedTemplate),
            "lua" => Some(LanguageId::Lua),
            "scala" | "sc" => Some(LanguageId::Scala),
            "sh" | "bash" => Some(LanguageId::Bash),
            "json" => Some(LanguageId::Json),
            _ => None,
        }
    }

    /// Get the LanguageConfig for this language
    ///
    /// Provides access to the full language configuration including
    /// extensions and query patterns.
    pub fn config(&self) -> &'static crate::parse::traits::LanguageConfig {
        match self {
            LanguageId::Python => &crate::parse::traits::languages::python::CONFIG,
            LanguageId::JavaScript => &crate::parse::traits::languages::javascript::CONFIG,
            LanguageId::TypeScript => &crate::parse::traits::languages::typescript::CONFIG,
            LanguageId::Go => &crate::parse::traits::languages::go::CONFIG,
            LanguageId::Rust => &crate::parse::traits::languages::rust::CONFIG,
            LanguageId::Java => &crate::parse::traits::languages::java::CONFIG,
            LanguageId::Cpp => &crate::parse::traits::languages::cpp::CONFIG,
            LanguageId::CSharp => &crate::parse::traits::languages::csharp::CONFIG,
            LanguageId::Ruby => &crate::parse::traits::languages::ruby::CONFIG,
            LanguageId::Php => &crate::parse::traits::languages::php::CONFIG,
            LanguageId::Lua => &crate::parse::traits::languages::lua::CONFIG,
            LanguageId::Scala => &crate::parse::traits::languages::scala::CONFIG,
            LanguageId::C => &crate::parse::traits::languages::c::CONFIG,
            LanguageId::Bash => &crate::parse::traits::languages::bash::CONFIG,
            LanguageId::Json => &crate::parse::traits::languages::json::CONFIG,
            LanguageId::Swift => &crate::parse::traits::languages::swift::CONFIG,
            LanguageId::Kotlin => &crate::parse::traits::languages::kotlin::CONFIG,
            LanguageId::Dart => &crate::parse::traits::languages::dart::CONFIG,
            LanguageId::Html => &crate::parse::traits::languages::html::CONFIG,
            LanguageId::Css => &crate::parse::traits::languages::css::CONFIG,
            LanguageId::Scss => &crate::parse::traits::languages::scss::CONFIG,
            LanguageId::Yaml => &crate::parse::traits::languages::yaml::CONFIG,
            LanguageId::Cmake => &crate::parse::traits::languages::cmake::CONFIG,
            LanguageId::Elixir => &crate::parse::traits::languages::elixir::CONFIG,
            LanguageId::Erlang => &crate::parse::traits::languages::erlang::CONFIG,
            LanguageId::Haskell => &crate::parse::traits::languages::haskell::CONFIG,
            LanguageId::Perl => &crate::parse::traits::languages::perl::CONFIG,
            LanguageId::R => &crate::parse::traits::languages::r::CONFIG,
            LanguageId::Zig => &crate::parse::traits::languages::zig::CONFIG,
            LanguageId::Graphql => &crate::parse::traits::languages::graphql::CONFIG,
            LanguageId::Hcl => &crate::parse::traits::languages::hcl::CONFIG,
            LanguageId::Make => &crate::parse::traits::languages::make::CONFIG,
            LanguageId::Elisp => &crate::parse::traits::languages::elisp::CONFIG,
            LanguageId::Julia => &crate::parse::traits::languages::julia::CONFIG,
            LanguageId::D => &crate::parse::traits::languages::d::CONFIG,
            LanguageId::Glsl => &crate::parse::traits::languages::glsl::CONFIG,
            LanguageId::EmbeddedTemplate => {
                &crate::parse::traits::languages::embedded_template::CONFIG
            }
        }
    }

    /// Load the tree-sitter Language for this LanguageId
    ///
    /// Uses the centralized language loading functions from traits::languages
    /// to avoid duplicate unsafe FFI declarations.
    fn load_language(&self) -> Language {
        match self {
            LanguageId::Python => crate::parse::traits::languages::python::language(),
            LanguageId::JavaScript => crate::parse::traits::languages::javascript::language(),
            LanguageId::TypeScript => crate::parse::traits::languages::typescript::language(),
            LanguageId::Go => crate::parse::traits::languages::go::language(),
            LanguageId::Rust => crate::parse::traits::languages::rust::language(),
            LanguageId::Java => crate::parse::traits::languages::java::language(),
            LanguageId::Cpp => crate::parse::traits::languages::cpp::language(),
            LanguageId::CSharp => crate::parse::traits::languages::csharp::language(),
            LanguageId::Ruby => crate::parse::traits::languages::ruby::language(),
            LanguageId::Php => crate::parse::traits::languages::php::language(),
            LanguageId::Lua => crate::parse::traits::languages::lua::language(),
            LanguageId::Scala => crate::parse::traits::languages::scala::language(),
            LanguageId::C => crate::parse::traits::languages::c::language(),
            LanguageId::Bash => crate::parse::traits::languages::bash::language(),
            LanguageId::Json => crate::parse::traits::languages::json::language(),
            LanguageId::Swift => crate::parse::traits::languages::swift::language(),
            LanguageId::Kotlin => crate::parse::traits::languages::kotlin::language(),
            LanguageId::Dart => crate::parse::traits::languages::dart::language(),
            LanguageId::Html => crate::parse::traits::languages::html::language(),
            LanguageId::Css => crate::parse::traits::languages::css::language(),
            LanguageId::Scss => crate::parse::traits::languages::scss::language(),
            LanguageId::Yaml => crate::parse::traits::languages::yaml::language(),
            LanguageId::Cmake => crate::parse::traits::languages::cmake::language(),
            LanguageId::Elixir => crate::parse::traits::languages::elixir::language(),
            LanguageId::Erlang => crate::parse::traits::languages::erlang::language(),
            LanguageId::Haskell => crate::parse::traits::languages::haskell::language(),
            LanguageId::Perl => crate::parse::traits::languages::perl::language(),
            LanguageId::R => crate::parse::traits::languages::r::language(),
            LanguageId::Zig => crate::parse::traits::languages::zig::language(),
            LanguageId::Graphql => crate::parse::traits::languages::graphql::language(),
            LanguageId::Hcl => crate::parse::traits::languages::hcl::language(),
            LanguageId::Make => crate::parse::traits::languages::make::language(),
            LanguageId::Elisp => crate::parse::traits::languages::elisp::language(),
            LanguageId::Julia => crate::parse::traits::languages::julia::language(),
            LanguageId::D => crate::parse::traits::languages::d::language(),
            LanguageId::Glsl => crate::parse::traits::languages::glsl::language(),
            LanguageId::EmbeddedTemplate => {
                crate::parse::traits::languages::embedded_template::language()
            }
        }
    }

    /// Get the language from the global cache (lazy-loaded)
    ///
    /// This is the primary method for obtaining a Language object.
    /// It uses lazy loading via the global cache.
    pub fn from_cache(&self) -> Result<Language, crate::parse::traits::Error> {
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
        let lang = cache.get_or_load(0, crate::parse::traits::languages::python::language);
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
        assert_eq!(
            LanguageId::from_extension("js"),
            Some(LanguageId::JavaScript)
        );
        assert_eq!(
            LanguageId::from_extension("jsx"),
            Some(LanguageId::JavaScript)
        );
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
