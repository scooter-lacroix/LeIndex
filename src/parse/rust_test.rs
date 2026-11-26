use super::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_function_extraction() {
        let source = b"fn greet(name: &str) -> String {
    format!(\"Hello, {}\", name)
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "greet");
        assert_eq!(sig.parameters.len(), 1);
        assert_eq!(sig.parameters[0].name, "name");
        assert_eq!(sig.return_type, Some("String".to_string()));
        assert!(!sig.is_method);
    }

    /// Calls made inside macro token trees must reach the call graph.
    ///
    /// `assert_eq!(callee(1), 2)` parses its body as flat tokens, so before
    /// the macro re-parse the only recorded call here was `assert_eq` and
    /// `callee`/`plain_call` had no callers at all — which silently emptied
    /// every caller list for code exercised through assert!/println!-style
    /// macros (that is, virtually every test function).
    #[test]
    fn test_calls_inside_macros_reach_the_call_list() {
        let source = b"fn callee(x: u32) -> u32 { x }\n\
fn outer() {\n\
    assert_eq!(callee(1), 2);\n\
    println!(\"{} {}\", callee(3), plain_call());\n\
}\n\
fn plain_call() -> u32 { 7 }\n";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let outer = signatures
            .iter()
            .find(|sig| sig.name == "outer")
            .expect("outer fn extracted");
        assert!(
            outer.calls.iter().any(|call| call == "callee"),
            "call inside assert_eq! must be recorded, calls: {:?}",
            outer.calls
        );
        assert!(
            outer.calls.iter().any(|call| call == "plain_call"),
            "call inside println! must be recorded, calls: {:?}",
            outer.calls
        );
    }

    /// A token tree that is not valid Rust expression syntax (a custom DSL
    /// macro body) must not invent calls from garbage, and must not wedge the
    /// extractor: the DSL tokens are simply not recorded.
    #[test]
    fn test_dsl_macro_body_does_not_invent_calls() {
        let source = b"fn dsl_user() {\n\
    sql_query!(SELECT name FROM users WHERE id = 3);\n\
    real_call();\n\
}\n\
fn real_call() {}\n";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let dsl_user = signatures
            .iter()
            .find(|sig| sig.name == "dsl_user")
            .expect("dsl_user fn extracted");
        // `SELECT name FROM users WHERE id` must not decay into call entries;
        // the ordinary call right next to it must still be recorded.
        assert!(
            !dsl_user
                .calls
                .iter()
                .any(|call| call != "sql_query" && call != "real_call"),
            "DSL macro body leaked pseudo-calls: {:?}",
            dsl_user.calls
        );
        assert!(dsl_user.calls.iter().any(|call| call == "real_call"));
    }

    /// `stringify!(leaf())` produces text; it does not call `leaf`. Recording
    /// `leaf` from the macro body gave a same-named project function a false
    /// `Call` edge, which then leaked into caller lists and test lookups.
    /// Macros that DO evaluate their arguments (`format!`, `assert!`) keep
    /// recording theirs.
    #[test]
    fn test_stringify_body_is_not_recorded_as_calls() {
        let source = b"fn leaf() -> u32 { 1 }\n\
fn quoted() -> &'static str {\n\
    std::stringify!(leaf())\n\
}\n\
fn evaluated() -> String {\n\
    format!(\"{}\", leaf())\n\
}\n";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let quoted = signatures
            .iter()
            .find(|sig| sig.name == "quoted")
            .expect("quoted fn extracted");
        assert!(
            !quoted.calls.iter().any(|call| call == "leaf"),
            "stringify! does not evaluate its body, calls: {:?}",
            quoted.calls
        );
        assert!(
            quoted.calls.iter().any(|call| call.contains("stringify")),
            "the macro itself is still a call: {:?}",
            quoted.calls
        );
        let evaluated = signatures
            .iter()
            .find(|sig| sig.name == "evaluated")
            .expect("evaluated fn extracted");
        assert!(
            evaluated.calls.iter().any(|call| call == "leaf"),
            "format! evaluates its arguments, calls: {:?}",
            evaluated.calls
        );
    }

    /// The repeat form `vec![make_value(); len()]` is valid Rust, but the
    /// extracted token tree keeps its own delimiter (`(...)`, `vec!(…)` is
    /// equally valid), and every reparse wrapper then leaves the `;` inside
    /// parentheses, so no candidate parsed and `make_value`/`len` vanished.
    /// Normalising the outer delimiter to brackets makes it an array
    /// expression. The invalid-DSL control still records nothing.
    #[test]
    fn test_vec_repeat_form_calls_are_recovered_regardless_of_delimiter() {
        for (delimiter_open, delimiter_close) in [("[", "]"), ("(", ")")] {
            let source = format!(
                "fn make_value() -> u32 {{ 1 }}\n\
                 fn len() -> usize {{ 1 }}\n\
                 fn build() {{\n\
                     let _ = vec{open}make_value(); len(){close};\n\
                     sql_query!(SELECT name FROM users);\n\
                 }}\n",
                open = delimiter_open,
                close = delimiter_close
            );

            let parser = RustParser::new();
            let signatures = parser.get_signatures(source.as_bytes()).unwrap();
            let build = signatures
                .iter()
                .find(|sig| sig.name == "build")
                .expect("build fn extracted");
            assert!(
                build.calls.iter().any(|call| call == "make_value"),
                "vec! repeat element call missing ({delimiter_open}{delimiter_close}): {:?}",
                build.calls
            );
            assert!(
                build.calls.iter().any(|call| call == "len"),
                "vec! repeat length call missing ({delimiter_open}{delimiter_close}): {:?}",
                build.calls
            );
            assert!(
                !build
                    .calls
                    .iter()
                    .any(|call| call.contains("SELECT") || call == "name" || call == "users"),
                "DSL macro leaked pseudo-calls: {:?}",
                build.calls
            );
        }
    }

    #[test]
    fn test_rust_async_function() {
        let source = b"async fn fetch_data(url: &str) -> Result<String, Error> {
    Ok(String::new())
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "fetch_data");
        assert!(sig.is_async);
    }

    #[test]
    fn test_rust_method_extraction() {
        let source = b"impl Server {
    fn new() -> Self {
        Server {}
    }

    pub fn start(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl Client for Server {
    fn connect(&self) -> bool {
        true
    }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should find methods from impl blocks
        let methods: Vec<_> = signatures.iter().filter(|s| s.is_method).collect();
        assert!(!methods.is_empty());
        assert!(
            methods
                .iter()
                .any(|sig| sig.qualified_name == "Server::new")
        );
        assert!(
            methods
                .iter()
                .any(|sig| sig.qualified_name == "Server::start")
        );
        assert!(
            methods
                .iter()
                .any(|sig| sig.qualified_name == "Server::connect")
        );
    }

    #[test]
    fn test_rust_struct_extraction() {
        let source = b"struct Point {
    x: f64,
    y: f64,
}

pub struct Person {
    pub name: String,
    pub age: u32,
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let point = signatures.iter().find(|s| s.name == "Point");
        assert!(point.is_some());

        let person = signatures.iter().find(|s| s.name == "Person");
        assert!(person.is_some());
    }

    #[test]
    fn test_rust_enum_extraction() {
        let source = b"enum Option<T> {
    Some(T),
    None,
}

pub enum Result<T, E> {
    Ok(T),
    Err(E),
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert!(signatures.len() >= 2);

        let option = signatures.iter().find(|s| s.name == "Option");
        assert!(option.is_some());

        let result = signatures.iter().find(|s| s.name == "Result");
        assert!(result.is_some());
        assert!(
            signatures
                .iter()
                .any(|s| s.qualified_name == "Option::Some")
        );
        assert!(
            signatures
                .iter()
                .any(|s| s.qualified_name == "Option::None")
        );
    }

    #[test]
    fn test_rust_trait_extraction() {
        let source = b"trait Display {
    fn fmt(&self, f: &mut Formatter) -> Result;
}

trait Iterator {
    type Item;
    fn next(&mut self) -> Option<Self::Item>;
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract trait declarations
        assert!(signatures.len() >= 2);

        let display = signatures.iter().find(|s| s.name == "Display");
        assert!(display.is_some());

        let iterator = signatures.iter().find(|s| s.name == "Iterator");
        assert!(iterator.is_some());
    }

    #[test]
    fn test_rust_visibility_modifiers() {
        let source = b"pub fn public_function() {}

fn private_function() {}

pub(crate) fn crate_function() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        let public = signatures.iter().find(|s| s.name == "public_function");
        assert_eq!(public.unwrap().visibility, Visibility::Public);

        let private = signatures.iter().find(|s| s.name == "private_function");
        assert_eq!(private.unwrap().visibility, Visibility::Private);

        let crate_fn = signatures.iter().find(|s| s.name == "crate_function");
        assert!(crate_fn.is_some());
    }

    #[test]
    fn test_rust_import_extraction() {
        let source = b"use std::collections::HashMap;
use crate::module::Item;

fn main() {}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Should extract use declarations
        let imports: Vec<_> = signatures
            .iter()
            .filter(|s| s.return_type.as_deref() == Some("use"))
            .collect();

        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_rust_self_parameter() {
        let source = b"impl Foo {
    fn by_ref(&self) -> i32 { 0 }
    fn by_mut_ref(&mut self) -> i32 { 0 }
    fn by_value(self) -> i32 { 0 }
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 3);

        for sig in &signatures {
            assert!(sig.is_method);
            assert!(!sig.parameters.is_empty());
            assert!(sig.parameters[0].name.contains("self"));
        }
    }

    #[test]
    fn test_rust_complexity_calculation() {
        let source = b"fn complex(x: i32) -> i32 {
    if x > 0 {
        for i in 0..x {
            if i % 2 == 0 {
                println!(\"{}\", i);
            }
        }
    }
    x
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        assert!(metrics.cyclomatic > 1);
        assert!(metrics.nesting_depth > 0);
    }

    #[test]
    fn test_rust_complexity_with_boolean_operators() {
        let source = b"fn boolean_ops(x: i32, y: i32) -> bool {
    if x > 0 && y > 0 {
        return true;
    }
    if x < 0 || y < 0 {
        return false;
    }
    x > 0 && y > 0 || x == 0 && y == 0
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 if expressions (2) + 5 boolean operators (5) = 8
        assert!(
            metrics.cyclomatic >= 8,
            "Expected cyclomatic complexity >= 8, got {}",
            metrics.cyclomatic
        );
    }

    #[test]
    fn test_rust_complexity_with_try_expressions() {
        let source = b"fn try_ops(result: Result<i32, Error>) -> Result<i32, Error> {
    let x = result?;
    let y = Some(2).ok_or(Error::NotFound)?;
    Ok(x + y)
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 try expressions (2) = 3
        assert!(
            metrics.cyclomatic >= 3,
            "Expected cyclomatic complexity >= 3, got {}",
            metrics.cyclomatic
        );
    }

    #[test]
    fn test_rust_complexity_combined() {
        let source = b"fn combined(x: i32, y: i32) -> i32 {
    if x > 0 && y > 0 {
        return x + y;
    }
    if x < 0 || y < 0 {
        return x - y;
    }
    0
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 2 if expressions (2) + 2 boolean operators (2) = 5
        // Note: tree-sitter might parse complex boolean expressions as nested binary expressions
        assert_eq!(
            metrics.cyclomatic, 5,
            "Expected cyclomatic complexity = 5, got {}",
            metrics.cyclomatic
        );
    }

    #[test]
    fn test_rust_complexity_with_try_and_bool() {
        let source =
            b"fn try_and_bool(x: Result<i32, Error>, y: Result<i32, Error>) -> Result<i32, Error> {
    let a = x?;
    let b = y?;
    if a > 0 && b > 0 {
        return Ok(a + b);
    }
    Ok(a - b)
}";

        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let root = tree.root_node();

        let rust_parser = RustParser::new();
        let metrics = rust_parser.extract_complexity(&root);

        // Base complexity (1) + 1 if expression (1) + 1 boolean operator (1) + 2 try expressions (2) = 5
        assert_eq!(
            metrics.cyclomatic, 5,
            "Expected cyclomatic complexity = 5, got {}",
            metrics.cyclomatic
        );
    }

    #[test]
    fn test_rust_cyclomatic_complexity_populated_in_signature() {
        let source = b"fn complex_fn(x: i32) -> i32 {
    if x > 0 {
        return x * 2;
    }
    x + 1
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        assert_eq!(signatures.len(), 1);
        let sig = &signatures[0];
        assert_eq!(sig.name, "complex_fn");

        // Verify that cyclomatic_complexity is populated
        // The function has 1 if expression, so cyclomatic complexity should be 2 (base 1 + 1 if)
        assert!(
            sig.cyclomatic_complexity >= 1,
            "cyclomatic_complexity should be >= 1, got {}",
            sig.cyclomatic_complexity
        );
    }

    #[test]
    fn test_rust_struct_instantiation_calls() {
        let source = b"fn create_structs() {
    let x = MyStruct { field: 1 };
    let y = AnotherStruct { value: compute() };
    let z = GenericStruct::<i32> { data: 42 };
}

fn compute() -> i32 {
    42
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let create_structs_fn = signatures.iter().find(|s| s.name == "create_structs");
        assert!(
            create_structs_fn.is_some(),
            "create_structs function not found"
        );

        let fn_sig = create_structs_fn.unwrap();

        // Should detect struct instantiations
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("MyStruct")),
            "Should detect MyStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("AnotherStruct")),
            "Should detect AnotherStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("GenericStruct")),
            "Should detect GenericStruct instantiation, got calls: {:?}",
            fn_sig.calls
        );

        // Should also detect function calls within struct fields
        assert!(
            fn_sig.calls.iter().any(|c| c.contains("compute")),
            "Should detect compute() call within struct field, got calls: {:?}",
            fn_sig.calls
        );
    }

    #[test]
    fn test_rust_scoped_identifier_extraction() {
        let source = b"
struct DeepThoughtManager {
    answer: i32,
}

impl DeepThoughtManager {
    fn new() -> Self {
        DeepThoughtManager { answer: 42 }
    }
}

fn test_function() {
    let manager = DeepThoughtManager::new();
    let another = DeepThoughtManager::new();
}";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        let test_fn = signatures.iter().find(|s| s.name == "test_function");
        assert!(test_fn.is_some(), "test_function not found");

        let fn_sig = test_fn.unwrap();

        // Should detect the scoped function call DeepThoughtManager::new
        assert!(
            fn_sig
                .calls
                .iter()
                .any(|c| c.contains("DeepThoughtManager::new")),
            "Should detect DeepThoughtManager::new call, got calls: {:?}",
            fn_sig.calls
        );

        // Should also detect just the type prefix DeepThoughtManager
        assert!(
            fn_sig.calls.iter().any(|c| c == "DeepThoughtManager"),
            "Should detect DeepThoughtManager type prefix, got calls: {:?}",
            fn_sig.calls
        );
    }

    #[test]
    fn test_rust_mod_item_extraction() {
        let source = b"
pub mod outer {
    mod inner {
        fn baz() {}
    }
}
mod external;
";

        let parser = RustParser::new();
        let signatures = parser.get_signatures(source).unwrap();

        // Module signatures should be present
        let outer = signatures.iter().find(|s| s.name == "outer");
        assert!(outer.is_some(), "should emit SignatureInfo for `outer`");
        assert_eq!(outer.unwrap().return_type.as_deref(), Some("module"));
        assert_eq!(outer.unwrap().visibility, Visibility::Public);

        let inner = signatures.iter().find(|s| s.name == "inner");
        assert!(
            inner.is_some(),
            "should emit SignatureInfo for nested `inner`"
        );
        assert_eq!(inner.unwrap().return_type.as_deref(), Some("module"));

        // `mod external;` should also produce a module signature
        let ext = signatures.iter().find(|s| s.name == "external");
        assert!(
            ext.is_some(),
            "should emit SignatureInfo for external `mod external;`"
        );

        // Function inside nested module should be indexed
        let baz = signatures.iter().find(|s| s.name == "baz");
        assert!(baz.is_some(), "nested fn baz should be indexed");
        // qualified_name should reflect the module path exactly
        assert_eq!(
            baz.unwrap().qualified_name,
            "outer::inner::baz",
            "baz qualified_name should match the module path exactly"
        );
    }

    #[test]
    fn test_rust_flow_facts_capture_command_and_state_channels() {
        let source = br#"
fn execute_native_command(password: &str, askpass: &str) {
    let mut command = std::process::Command::new("sudo");
    command.arg("-S").env("SUDO_ASKPASS", askpass).stdin(password);
    registry_record(password);
    verify_installation();
}
"#;
        let signatures = RustParser::new().get_signatures(source).unwrap();
        let sig = signatures
            .iter()
            .find(|sig| sig.name == "execute_native_command")
            .unwrap();

        assert!(
            sig.flow_facts.iter().any(|fact| {
                fact.channel == FlowChannel::CommandArgument && fact.target == "argv"
            })
        );
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::Environment && fact.target == "SUDO_ASKPASS"
        }));
        assert!(
            sig.flow_facts
                .iter()
                .any(|fact| { fact.channel == FlowChannel::Stdin && fact.target == "password" })
        );
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::StateWrite && fact.target == "registry_record"
        }));
        assert!(sig.flow_facts.iter().any(|fact| {
            fact.channel == FlowChannel::StateRead && fact.target == "verify_installation"
        }));
    }
}
