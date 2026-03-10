// Language-specific parser implementations

pub use crate::parse::bash::BashParser;
pub use crate::parse::c::CParser;
pub use crate::parse::cpp::CppParser;
pub use crate::parse::csharp::CSharpParser;
pub use crate::parse::go::GoParser;
pub use crate::parse::java::JavaParser;
pub use crate::parse::javascript::{JavaScriptParser, TypeScriptParser};
pub use crate::parse::json::JsonParser;
pub use crate::parse::lua::LuaParser;
pub use crate::parse::php::PhpParser;
pub use crate::parse::python::PythonParser;
pub use crate::parse::ruby::RubyParser;
pub use crate::parse::rust::RustParser;
pub use crate::parse::scala::ScalaParser;

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
        _ => None,
    }
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
