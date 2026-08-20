// Language-specific parser implementations

pub use crate::parse::bash::BashParser;
pub use crate::parse::c::CParser;
pub use crate::parse::cpp::CppParser;
pub use crate::parse::csharp::CSharpParser;
pub use crate::parse::dart::DartParser;
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

/// Type-specific parser factory
pub fn parser_for_language(
    language: &str,
) -> Option<Box<dyn crate::parse::traits::CodeIntelligence>> {
    match language.to_lowercase().as_str() {
        "python" | "py" => Some(Box::new(PythonParser::new())),
        "javascript" | "js" => Some(Box::new(JavaScriptParser::new())),
        "typescript" | "ts" => Some(Box::new(TypeScriptParser::new())),
        "rust" | "rs" => Some(Box::new(RustParser::new())),
        "go" => Some(Box::new(GoParser::new())),
        "java" => Some(Box::new(JavaParser::new())),
        "cpp" | "c++" => Some(Box::new(CppParser::new())),
        "csharp" | "c#" => Some(Box::new(CSharpParser::new())),
        "ruby" | "rb" => Some(Box::new(RubyParser::new())),
        "php" => Some(Box::new(PhpParser::new())),
        "lua" => Some(Box::new(LuaParser::new())),
        "scala" => Some(Box::new(ScalaParser::new())),
        "c" => Some(Box::new(CParser::new())),
        "bash" | "sh" => Some(Box::new(BashParser::new())),
        "json" => Some(Box::new(JsonParser::new())),
        "swift" => Some(Box::new(SwiftParser::new())),
        "kotlin" | "kt" => Some(Box::new(KotlinParser::new())),
        "dart" => Some(Box::new(DartParser::new())),
        "html" | "htm" => Some(Box::new(GenericParser::new("html"))),
        "css" => Some(Box::new(GenericParser::new("css"))),
        "scss" => Some(Box::new(GenericParser::new("scss"))),
        "yaml" | "yml" => Some(Box::new(GenericParser::new("yaml"))),
        "cmake" => Some(Box::new(GenericParser::new("cmake"))),
        "elixir" | "ex" | "exs" => Some(Box::new(GenericParser::new("elixir"))),
        "erlang" | "erl" => Some(Box::new(GenericParser::new("erlang"))),
        "haskell" | "hs" => Some(Box::new(GenericParser::new("haskell"))),
        "perl" | "pl" | "pm" => Some(Box::new(GenericParser::new("perl"))),
        "r" => Some(Box::new(GenericParser::new("r"))),
        "zig" => Some(Box::new(GenericParser::new("zig"))),
        "graphql" | "gql" => Some(Box::new(GenericParser::new("graphql"))),
        "hcl" | "terraform" | "tf" => Some(Box::new(GenericParser::new("hcl"))),
        "make" | "makefile" => Some(Box::new(GenericParser::new("make"))),
        "elisp" | "emacs-lisp" => Some(Box::new(GenericParser::new("elisp"))),
        "julia" | "jl" => Some(Box::new(GenericParser::new("julia"))),
        "d" => Some(Box::new(GenericParser::new("d"))),
        "glsl" => Some(Box::new(GenericParser::new("glsl"))),
        "embedded-template" | "ejs" | "erb" | "liquid" => {
            Some(Box::new(GenericParser::new("embedded_template")))
        }
        _ => None,
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
}
