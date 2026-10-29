// Language-specific parser implementations

pub use crate::parse::bash::BashParser;
pub use crate::parse::c::CParser;
pub use crate::parse::cpp::CppParser;
pub use crate::parse::csharp::CSharpParser;
pub use crate::parse::dart::DartParser;
pub use crate::parse::docs::{DocFlavor, DocParser};
pub use crate::parse::generic::GenericParser;
pub use crate::parse::go::GoParser;
pub use crate::parse::java::JavaParser;
pub use crate::parse::javascript::{JavaScriptParser, TypeScriptParser};
pub use crate::parse::json::JsonParser;
pub use crate::parse::kotlin::KotlinParser;
pub use crate::parse::lua::LuaParser;
pub use crate::parse::php::PhpParser;
pub use crate::parse::python::PythonParser;
pub use crate::parse::ruby::RubyParser;
pub use crate::parse::rust::RustParser;
pub use crate::parse::scala::ScalaParser;
pub use crate::parse::swift::SwiftParser;

/// Language name → parser constructor (#87: data over a 47-arm match, which
/// lizard 1.24.x scores CCN 42). Keys are LOWERCASED names, including every
/// alias and display form the old match accepted. Keys must stay unique:
/// the match this table replaced guaranteed that at compile time, and
/// `test_parser_table_keys_are_unique` keeps the property.
const PARSER_TABLE: &[(
    &str,
    fn() -> Box<dyn crate::parse::traits::CodeIntelligence>,
)] = &[
    ("python", || Box::new(PythonParser::new())),
    ("py", || Box::new(PythonParser::new())),
    ("javascript", || Box::new(JavaScriptParser::new())),
    ("js", || Box::new(JavaScriptParser::new())),
    ("typescript", || Box::new(TypeScriptParser::new())),
    ("ts", || Box::new(TypeScriptParser::new())),
    ("rust", || Box::new(RustParser::new())),
    ("rs", || Box::new(RustParser::new())),
    ("go", || Box::new(GoParser::new())),
    ("java", || Box::new(JavaParser::new())),
    ("cpp", || Box::new(CppParser::new())),
    ("c++", || Box::new(CppParser::new())),
    ("csharp", || Box::new(CSharpParser::new())),
    ("c#", || Box::new(CSharpParser::new())),
    ("ruby", || Box::new(RubyParser::new())),
    ("rb", || Box::new(RubyParser::new())),
    ("php", || Box::new(PhpParser::new())),
    ("lua", || Box::new(LuaParser::new())),
    ("scala", || Box::new(ScalaParser::new())),
    ("c", || Box::new(CParser::new())),
    ("bash", || Box::new(BashParser::new())),
    ("sh", || Box::new(BashParser::new())),
    ("json", || Box::new(JsonParser::new())),
    ("swift", || Box::new(SwiftParser::new())),
    ("kotlin", || Box::new(KotlinParser::new())),
    ("kt", || Box::new(KotlinParser::new())),
    ("dart", || Box::new(DartParser::new())),
    ("html", || Box::new(GenericParser::new("html"))),
    ("htm", || Box::new(GenericParser::new("html"))),
    ("css", || Box::new(GenericParser::new("css"))),
    ("scss", || Box::new(GenericParser::new("scss"))),
    ("yaml", || Box::new(GenericParser::new("yaml"))),
    ("yml", || Box::new(GenericParser::new("yaml"))),
    ("cmake", || Box::new(GenericParser::new("cmake"))),
    ("elixir", || Box::new(GenericParser::new("elixir"))),
    ("ex", || Box::new(GenericParser::new("elixir"))),
    ("exs", || Box::new(GenericParser::new("elixir"))),
    ("erlang", || Box::new(GenericParser::new("erlang"))),
    ("erl", || Box::new(GenericParser::new("erlang"))),
    ("haskell", || Box::new(GenericParser::new("haskell"))),
    ("hs", || Box::new(GenericParser::new("haskell"))),
    ("perl", || Box::new(GenericParser::new("perl"))),
    ("pl", || Box::new(GenericParser::new("perl"))),
    ("pm", || Box::new(GenericParser::new("perl"))),
    ("r", || Box::new(GenericParser::new("r"))),
    ("zig", || Box::new(GenericParser::new("zig"))),
    ("graphql", || Box::new(GenericParser::new("graphql"))),
    ("gql", || Box::new(GenericParser::new("graphql"))),
    ("hcl", || Box::new(GenericParser::new("hcl"))),
    ("terraform", || Box::new(GenericParser::new("hcl"))),
    ("tf", || Box::new(GenericParser::new("hcl"))),
    ("make", || Box::new(GenericParser::new("make"))),
    ("makefile", || Box::new(GenericParser::new("make"))),
    ("elisp", || Box::new(GenericParser::new("elisp"))),
    ("emacs-lisp", || Box::new(GenericParser::new("elisp"))),
    ("emacs lisp", || Box::new(GenericParser::new("elisp"))),
    ("julia", || Box::new(GenericParser::new("julia"))),
    ("jl", || Box::new(GenericParser::new("julia"))),
    ("d", || Box::new(GenericParser::new("d"))),
    ("glsl", || Box::new(GenericParser::new("glsl"))),
    ("embedded-template", || {
        Box::new(GenericParser::new("embedded_template"))
    }),
    ("embedded template", || {
        Box::new(GenericParser::new("embedded_template"))
    }),
    ("ejs", || Box::new(GenericParser::new("embedded_template"))),
    ("erb", || Box::new(GenericParser::new("embedded_template"))),
    ("liquid", || {
        Box::new(GenericParser::new("embedded_template"))
    }),
    ("markdown", || Box::new(DocParser::new(DocFlavor::Markdown))),
    ("md", || Box::new(DocParser::new(DocFlavor::Markdown))),
    ("rst", || Box::new(DocParser::new(DocFlavor::Rst))),
    ("restructuredtext", || {
        Box::new(DocParser::new(DocFlavor::Rst))
    }),
    ("asciidoc", || Box::new(DocParser::new(DocFlavor::Adoc))),
    ("adoc", || Box::new(DocParser::new(DocFlavor::Adoc))),
    // `parallel.rs` resolves parsers by the language's DISPLAY name, so
    // the label forms ("Plain Text", "Emacs Lisp") must resolve too.
    ("text", || Box::new(DocParser::new(DocFlavor::Plain))),
    ("txt", || Box::new(DocParser::new(DocFlavor::Plain))),
    ("plaintext", || Box::new(DocParser::new(DocFlavor::Plain))),
    ("plain text", || Box::new(DocParser::new(DocFlavor::Plain))),
    ("plain", || Box::new(DocParser::new(DocFlavor::Plain))),
];

/// Type-specific parser factory
pub fn parser_for_language(
    language: &str,
) -> Option<Box<dyn crate::parse::traits::CodeIntelligence>> {
    let key = language.to_lowercase();
    PARSER_TABLE
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, build)| build())
}

#[cfg(test)]
mod parser_table_tests {
    use super::*;

    /// #87 guard: parser keys stay unique — the match this table replaced
    /// guaranteed that at compile time via unreachable patterns.
    #[test]
    fn test_parser_table_keys_are_unique() {
        for (i, (key, _)) in PARSER_TABLE.iter().enumerate() {
            assert!(
                PARSER_TABLE.iter().take(i).all(|(other, _)| other != key),
                "duplicate parser key: {key}"
            );
        }
    }
}

/// Number of languages with a registered parser (Tier-0 breadth floor).
pub fn active_language_count() -> usize {
    [
        "python",
        "javascript",
        "typescript",
        "rust",
        "go",
        "java",
        "cpp",
        "csharp",
        "ruby",
        "php",
        "lua",
        "scala",
        "c",
        "bash",
        "json",
        "swift",
        "kotlin",
        "dart",
        "html",
        "css",
        "scss",
        "yaml",
        "cmake",
        "elixir",
        "erlang",
        "haskell",
        "perl",
        "r",
        "zig",
        "graphql",
        "hcl",
        "make",
        "elisp",
        "julia",
        "d",
        "glsl",
        "embedded-template",
    ]
    .iter()
    .filter(|language| parser_for_language(language).is_some())
    .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::traits::CodeIntelligence;

    #[test]
    fn test_python_parser_creation() {
        let parser = PythonParser::new();
        let source = b"def hello(): pass";
        let result = parser.get_signatures(source);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parser_factory() {
        let parser = parser_for_language("python");
        assert!(parser.is_some());

        let parser = parser_for_language("unknown");
        assert!(parser.is_none());
    }
    /// Completeness gate: every registered file extension must resolve to a
    /// parser THROUGH ITS DISPLAY NAME — `parallel.rs` looks parsers up by
    /// `LanguageConfig::name` (e.g. "Plain Text"), not by registry key. This
    /// class of bug shipped once: .txt files failed with "No parser found for
    /// language: Plain Text" because the registry arm only listed
    /// "text"|"txt"|"plaintext".
    #[test]
    fn test_every_registered_extension_resolves_a_parser_via_display_name() {
        let representative_extensions = [
            "py", "js", "ts", "go", "rs", "java", "cpp", "hpp", "h", "c", "cs", "rb", "php", "lua",
            "scala", "sh", "json", "swift", "kt", "dart", "html", "htm", "css", "scss", "yaml",
            "yml", "cmake", "ex", "exs", "erl", "hrl", "hs", "pl", "pm", "r", "zig", "graphql",
            "gql", "hcl", "tf", "tfvars", "makefile", "mak", "mk", "el", "jl", "d", "di", "glsl",
            "vert", "frag", "comp", "ejs", "erb", "liquid", "md", "markdown", "rst", "adoc", "txt",
        ];
        for ext in representative_extensions {
            let id = crate::parse::grammar::LanguageId::from_extension(ext)
                .unwrap_or_else(|| panic!("extension .{ext} has no LanguageId"));
            let name = id.config().name.clone();
            assert!(
                parser_for_language(&name).is_some(),
                "language display name {name:?} (from .{ext}) resolves to no parser"
            );
        }
    }
}
