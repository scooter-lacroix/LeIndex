// Prelude module - common imports for convenience

pub use crate::ast::{AstNode, ClassElement, FunctionElement, Import, ModuleElement, NodeType};
pub use crate::completeness::{score_languages, LanguageCompleteness};
pub use crate::languages::{parser_for_language, JavaScriptParser, PythonParser};
pub use crate::parallel::ParallelParser;
pub use crate::traits::{
    CodeIntelligence, ComplexityMetrics, Edge, EdgeType, Error, Graph, ImportInfo, LanguageConfig,
    Parameter, QueryPatterns, Result, SignatureInfo, Visibility,
};
