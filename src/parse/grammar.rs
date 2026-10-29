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
    /// Markdown documentation (docs tier)
    Markdown = 38,
    /// reStructuredText documentation (docs tier)
    Rst = 39,
    /// AsciiDoc documentation (docs tier)
    Adoc = 40,
    /// Plain text documentation (docs tier)
    PlainText = 41,
}

impl LanguageId {
    /// Every variant, in discriminant order. Drives the table-coverage tests
    /// and any caller that needs the full language enumeration.
    pub const ALL: [LanguageId; 41] = [
        LanguageId::Python,
        LanguageId::JavaScript,
        LanguageId::TypeScript,
        LanguageId::Go,
        LanguageId::Rust,
        LanguageId::Java,
        LanguageId::Cpp,
        LanguageId::CSharp,
        LanguageId::Ruby,
        LanguageId::Php,
        LanguageId::Lua,
        LanguageId::Scala,
        LanguageId::C,
        LanguageId::Bash,
        LanguageId::Json,
        LanguageId::Swift,
        LanguageId::Kotlin,
        LanguageId::Dart,
        LanguageId::Html,
        LanguageId::Css,
        LanguageId::Scss,
        LanguageId::Yaml,
        LanguageId::Cmake,
        LanguageId::Elixir,
        LanguageId::Erlang,
        LanguageId::Haskell,
        LanguageId::Perl,
        LanguageId::R,
        LanguageId::Zig,
        LanguageId::Graphql,
        LanguageId::Hcl,
        LanguageId::Make,
        LanguageId::Elisp,
        LanguageId::Julia,
        LanguageId::D,
        LanguageId::Glsl,
        LanguageId::EmbeddedTemplate,
        LanguageId::Markdown,
        LanguageId::Rst,
        LanguageId::Adoc,
        LanguageId::PlainText,
    ];
}

/// Extension → language mapping (#87: data over a 41-arm match, which lizard
/// 1.24.x scores CCN 43). Keys are LOWERCASED extensions; lookup is a linear
/// scan, which is fine at this size. Two invariants hold by construction:
/// keys are unique (the compiler rejects duplicate match patterns — this
/// table replaced them verbatim), and appending a variant WITHOUT adding its
/// rows here is caught by `test_extension_table_covers_every_variant`.
const EXT_TABLE: &[(&str, LanguageId)] = &[
    ("py", LanguageId::Python),
    ("js", LanguageId::JavaScript),
    ("mjs", LanguageId::JavaScript),
    ("cjs", LanguageId::JavaScript),
    ("jsx", LanguageId::JavaScript),
    ("ts", LanguageId::TypeScript),
    ("tsx", LanguageId::TypeScript),
    ("mts", LanguageId::TypeScript),
    ("cts", LanguageId::TypeScript),
    ("go", LanguageId::Go),
    ("rs", LanguageId::Rust),
    ("java", LanguageId::Java),
    ("cpp", LanguageId::Cpp),
    ("cc", LanguageId::Cpp),
    ("cxx", LanguageId::Cpp),
    ("hpp", LanguageId::Cpp),
    ("h", LanguageId::C),
    ("c", LanguageId::C),
    ("cs", LanguageId::CSharp),
    ("rb", LanguageId::Ruby),
    ("php", LanguageId::Php),
    ("swift", LanguageId::Swift),
    ("kt", LanguageId::Kotlin),
    ("kts", LanguageId::Kotlin),
    ("dart", LanguageId::Dart),
    ("html", LanguageId::Html),
    ("htm", LanguageId::Html),
    ("css", LanguageId::Css),
    ("scss", LanguageId::Scss),
    ("yaml", LanguageId::Yaml),
    ("yml", LanguageId::Yaml),
    ("cmake", LanguageId::Cmake),
    ("ex", LanguageId::Elixir),
    ("exs", LanguageId::Elixir),
    ("erl", LanguageId::Erlang),
    ("hrl", LanguageId::Erlang),
    ("hs", LanguageId::Haskell),
    ("pl", LanguageId::Perl),
    ("pm", LanguageId::Perl),
    ("r", LanguageId::R),
    ("zig", LanguageId::Zig),
    ("graphql", LanguageId::Graphql),
    ("gql", LanguageId::Graphql),
    ("hcl", LanguageId::Hcl),
    ("tf", LanguageId::Hcl),
    ("tfvars", LanguageId::Hcl),
    ("makefile", LanguageId::Make),
    ("mak", LanguageId::Make),
    ("mk", LanguageId::Make),
    ("el", LanguageId::Elisp),
    ("jl", LanguageId::Julia),
    ("d", LanguageId::D),
    ("di", LanguageId::D),
    ("glsl", LanguageId::Glsl),
    ("vert", LanguageId::Glsl),
    ("frag", LanguageId::Glsl),
    ("comp", LanguageId::Glsl),
    ("ejs", LanguageId::EmbeddedTemplate),
    ("erb", LanguageId::EmbeddedTemplate),
    ("liquid", LanguageId::EmbeddedTemplate),
    ("md", LanguageId::Markdown),
    ("markdown", LanguageId::Markdown),
    ("rst", LanguageId::Rst),
    ("adoc", LanguageId::Adoc),
    ("asciidoc", LanguageId::Adoc),
    ("txt", LanguageId::PlainText),
    ("lua", LanguageId::Lua),
    ("scala", LanguageId::Scala),
    ("sc", LanguageId::Scala),
    ("sh", LanguageId::Bash),
    ("bash", LanguageId::Bash),
    ("json", LanguageId::Json),
];

/// LanguageId → LanguageConfig accessor, indexed by discriminant (#87: data
/// over a 41-arm match, which lizard 1.24.x scores CCN 41). Rows are
/// closures because the underlying CONFIG statics are `once_cell::Lazy` —
/// const context cannot deref-coerce them directly. Row 23 is a RETIRED
/// cache slot no variant carries — the enum's numbering history explains
/// why it must stay vacant. A variant appended without extending this
/// table panics on first config lookup — and
/// `test_config_table_covers_every_variant` fails in CI first.
const CONFIG_TABLE: [Option<fn() -> &'static crate::parse::traits::LanguageConfig>; 42] = [
    Some(|| &*crate::parse::traits::languages::python::CONFIG),
    Some(|| &*crate::parse::traits::languages::javascript::CONFIG),
    Some(|| &*crate::parse::traits::languages::typescript::CONFIG),
    Some(|| &*crate::parse::traits::languages::go::CONFIG),
    Some(|| &*crate::parse::traits::languages::rust::CONFIG),
    Some(|| &*crate::parse::traits::languages::java::CONFIG),
    Some(|| &*crate::parse::traits::languages::cpp::CONFIG),
    Some(|| &*crate::parse::traits::languages::csharp::CONFIG),
    Some(|| &*crate::parse::traits::languages::ruby::CONFIG),
    Some(|| &*crate::parse::traits::languages::php::CONFIG),
    Some(|| &*crate::parse::traits::languages::lua::CONFIG),
    Some(|| &*crate::parse::traits::languages::scala::CONFIG),
    Some(|| &*crate::parse::traits::languages::c::CONFIG),
    Some(|| &*crate::parse::traits::languages::bash::CONFIG),
    Some(|| &*crate::parse::traits::languages::json::CONFIG),
    Some(|| &*crate::parse::traits::languages::swift::CONFIG),
    Some(|| &*crate::parse::traits::languages::kotlin::CONFIG),
    Some(|| &*crate::parse::traits::languages::dart::CONFIG),
    Some(|| &*crate::parse::traits::languages::html::CONFIG),
    Some(|| &*crate::parse::traits::languages::css::CONFIG),
    Some(|| &*crate::parse::traits::languages::scss::CONFIG),
    Some(|| &*crate::parse::traits::languages::yaml::CONFIG),
    Some(|| &*crate::parse::traits::languages::cmake::CONFIG),
    None, // retired discriminant 23: no variant carries it
    Some(|| &*crate::parse::traits::languages::elixir::CONFIG),
    Some(|| &*crate::parse::traits::languages::erlang::CONFIG),
    Some(|| &*crate::parse::traits::languages::haskell::CONFIG),
    Some(|| &*crate::parse::traits::languages::perl::CONFIG),
    Some(|| &*crate::parse::traits::languages::r::CONFIG),
    Some(|| &*crate::parse::traits::languages::zig::CONFIG),
    Some(|| &*crate::parse::traits::languages::graphql::CONFIG),
    Some(|| &*crate::parse::traits::languages::hcl::CONFIG),
    Some(|| &*crate::parse::traits::languages::make::CONFIG),
    Some(|| &*crate::parse::traits::languages::elisp::CONFIG),
    Some(|| &*crate::parse::traits::languages::julia::CONFIG),
    Some(|| &*crate::parse::traits::languages::d::CONFIG),
    Some(|| &*crate::parse::traits::languages::glsl::CONFIG),
    Some(|| &*crate::parse::traits::languages::embedded_template::CONFIG),
    Some(|| &*crate::parse::traits::languages::markdown::CONFIG),
    Some(|| &*crate::parse::traits::languages::rst::CONFIG),
    Some(|| &*crate::parse::traits::languages::adoc::CONFIG),
    Some(|| &*crate::parse::traits::languages::plaintext::CONFIG),
];

impl LanguageId {
    /// Get the LanguageId for a file extension
    ///
    /// This is the unified entry point for language detection.
    /// Delegates to LanguageConfig::from_extension for consistency.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_lowercase();
        EXT_TABLE
            .iter()
            .find(|(key, _)| *key == ext)
            .map(|(_, id)| *id)
    }

    /// Get the LanguageConfig for this language
    ///
    /// Provides access to the full language configuration including
    /// extensions and query patterns.
    pub fn config(&self) -> &'static crate::parse::traits::LanguageConfig {
        // Indexed by discriminant; row 23 is a RETIRED cache slot no variant
        // carries (see the enum's numbering history). A variant appended
        // without extending this table panics on first config lookup — and
        // test_config_table_covers_every_variant fails in CI first.
        CONFIG_TABLE[*self as usize].expect("LanguageId variant without a CONFIG_TABLE row")()
    }

    /// Load the tree-sitter Language for this LanguageId
    ///
    /// Uses the centralized language loading functions from traits::languages
    /// to avoid duplicate unsafe FFI declarations.
    fn load_language(&self) -> Language {
        // Indexed by discriminant; `None` rows: the RETIRED slot 23, and the
        // doc flavors (38..=41) which never install a grammar — they get the
        // json stand-in, exactly as the match this table replaced did.
        const LOADERS: [Option<fn() -> Language>; 42] = [
            Some(crate::parse::traits::languages::python::language),
            Some(crate::parse::traits::languages::javascript::language),
            Some(crate::parse::traits::languages::typescript::language),
            Some(crate::parse::traits::languages::go::language),
            Some(crate::parse::traits::languages::rust::language),
            Some(crate::parse::traits::languages::java::language),
            Some(crate::parse::traits::languages::cpp::language),
            Some(crate::parse::traits::languages::csharp::language),
            Some(crate::parse::traits::languages::ruby::language),
            Some(crate::parse::traits::languages::php::language),
            Some(crate::parse::traits::languages::lua::language),
            Some(crate::parse::traits::languages::scala::language),
            Some(crate::parse::traits::languages::c::language),
            Some(crate::parse::traits::languages::bash::language),
            Some(crate::parse::traits::languages::json::language),
            Some(crate::parse::traits::languages::swift::language),
            Some(crate::parse::traits::languages::kotlin::language),
            Some(crate::parse::traits::languages::dart::language),
            Some(crate::parse::traits::languages::html::language),
            Some(crate::parse::traits::languages::css::language),
            Some(crate::parse::traits::languages::scss::language),
            Some(crate::parse::traits::languages::yaml::language),
            Some(crate::parse::traits::languages::cmake::language),
            None, // retired discriminant 23: no variant carries it
            Some(crate::parse::traits::languages::elixir::language),
            Some(crate::parse::traits::languages::erlang::language),
            Some(crate::parse::traits::languages::haskell::language),
            Some(crate::parse::traits::languages::perl::language),
            Some(crate::parse::traits::languages::r::language),
            Some(crate::parse::traits::languages::zig::language),
            Some(crate::parse::traits::languages::graphql::language),
            Some(crate::parse::traits::languages::hcl::language),
            Some(crate::parse::traits::languages::make::language),
            Some(crate::parse::traits::languages::elisp::language),
            Some(crate::parse::traits::languages::julia::language),
            Some(crate::parse::traits::languages::d::language),
            Some(crate::parse::traits::languages::glsl::language),
            Some(crate::parse::traits::languages::embedded_template::language),
            None, // doc flavor: stand-in below
            None, // doc flavor: stand-in below
            None, // doc flavor: stand-in below
            None, // doc flavor: stand-in below
        ];
        LOADERS[*self as usize].map_or_else(
            || crate::parse::traits::languages::json::language(),
            |loader| loader(),
        )
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

    /// #87 table guards: every variant must have an extension row and a
    /// config-table row, and the extension keys must stay unique (the match
    /// this table replaced guaranteed that at compile time).
    #[test]
    fn test_extension_table_covers_every_variant() {
        for id in LanguageId::ALL {
            assert!(
                EXT_TABLE.iter().any(|(_, mapped)| *mapped == id),
                "{id:?} has no extension row in EXT_TABLE"
            );
        }
    }

    #[test]
    fn test_extension_table_keys_are_unique() {
        for (i, (key, _)) in EXT_TABLE.iter().enumerate() {
            assert!(
                EXT_TABLE.iter().take(i).all(|(other, _)| other != key),
                "duplicate extension key: {key}"
            );
        }
    }

    #[test]
    fn test_config_table_covers_every_variant() {
        for id in LanguageId::ALL {
            assert!(
                !id.config().name.is_empty(),
                "{id:?} has no config-table row (index = its discriminant)"
            );
        }
    }

    #[test]
    fn test_language_ids_keep_their_frozen_discriminants() {
        // GLOBAL_GRAMMAR_CACHE keys on `id as usize`; discriminants are a
        // storage ABI. Row 23 is a retired slot and must stay vacant: no
        // variant may claim it, and every variant after the gap is offset
        // by exactly one (Cmake=22 → Elixir=24).
        for (i, id) in LanguageId::ALL.iter().enumerate() {
            let expected = if i >= 23 { i + 1 } else { i };
            assert_eq!(
                *id as usize, expected,
                "{id:?} moved; discriminants are frozen"
            );
        }
        assert!(
            !LanguageId::ALL.iter().any(|id| *id as usize == 23),
            "discriminant 23 is the retired slot and must stay vacant"
        );
    }

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
